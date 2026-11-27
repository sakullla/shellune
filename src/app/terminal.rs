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
    App, Bounds, BoxShadow, ClipboardItem, Context, DispatchPhase, ElementInputHandler,
    EntityInputHandler, FocusHandle, FontWeight, KeyDownEvent, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Size, UTF16Selection, Window, canvas, div, point, px,
};
use std::ops::Range;

use shellune_core::{EventTag, LocalShell, SessionFailure, TerminalMode};
use shellune_ssh::{ChainEvent, ChainSession, RemoteEvent, RemoteSession, X11Status};
use shellune_terminal::core::{LineAnchor, SearchOptions};
use shellune_terminal::highlight::{LinkRefusal, is_openable_http_url, osc8_target_allowed};
use shellune_terminal::shell::ShellEnvironmentNote;
use shellune_terminal::{
    BlockFilter, BlockStatus, CellSize, CommandBlock, CommandBlockId, CursorShape, DetectedLink,
    ExitReport, GridSize, HistoryMatch, Key, KeyModifiers, LocalSession, MouseAction,
    MouseButton as TerminalMouseButton, MouseModifiers, PtyEvent, RenderedCell, Rgb, SelectionType,
    Side, Snapshot, TermMode, TerminalCore, TerminalEffects, encode_alternate_scroll, encode_key,
    encode_mouse, encode_paste, encode_text, grid_for, mouse_reporting, palette,
    paste_needs_confirmation, sanitize_font_size, step_font_size,
};
use shellune_terminal::{
    FinishedCommand, LinkKind, LogFormat, LogReport, ProgramNotice, SessionLog,
};

use crate::app::actions::{TerminalCopy, TerminalPaste, TerminalSearch};
use crate::app::components;
use crate::app::copy::Locale;
use crate::app::glyphs;
use crate::app::icons::Icon;
use crate::app::serial::{
    CloseReport, NewlineFormat, PortScan, SerialLink, SerialPane, SerialTarget, describe_for,
};
use crate::app::theme::{self, Metrics, Tone, palette as shell_palette, radius, spacing};

/// How often the pane looks for output from the shell.
///
/// Output is coalesced rather than drawn byte by byte: a program that writes
/// thousands of small chunks must not force thousands of frames, and 12ms is below
/// the point where typing feels delayed.
pub const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(12);

/// Let a busy SSH pane consume several transport batches before repainting. Check
/// the deadline between batches so keyboard and window events still get a turn.
const SSH_DRAIN_BUDGET: Duration = Duration::from_millis(8);

/// How close two measured lengths of the bar must be, in pixels, to count as the
/// same: layout rounding moves an edge by a fraction of a pixel between frames, and
/// that must not ask for another frame.
const LAYOUT_SLACK: f32 = 0.5;

/// The narrowest the session slot of the terminal bar becomes, in logical pixels
/// before zoom: its mark and a letter or two.
const SESSION_SLOT_MIN_WIDTH: f32 = 40.0;

/// The tallest the search results panel grows, in logical pixels before zoom, and the
/// floor it keeps at every text size.
const SEARCH_PANEL_MAX_HEIGHT: f32 = 180.0;

/// The ceiling on the search results panel at large text, so the grid stays readable
/// behind it.
const SEARCH_PANEL_MAX_HEIGHT_CAP: f32 = 260.0;

/// The narrowest the search query field becomes once it holds a query.
const SEARCH_QUERY_MIN_WIDTH: f32 = 80.0;
/// The widest the find bar's query field grows on a wide pane.
const SEARCH_FIELD_MAX_WIDTH: f32 = 360.0;

/// The soft ring outside the search query field while it has the keyboard.
const SEARCH_FOCUS_HALO: f32 = 2.0;

/// The gap between the scroll position thumb and the grid's right edge.
const SCROLL_THUMB_INSET: f32 = 2.0;

/// The width of the scroll position thumb.
const SCROLL_THUMB_WIDTH: f32 = 4.0;

/// The width of the scroll position thumb when pointed at.
const SCROLL_THUMB_HOVER_WIDTH: f32 = 6.0;

/// Hit-target width of the interactive scrollbar track along the right edge.
const SCROLL_TRACK_WIDTH: f32 = 12.0;

/// A selection a capture asked for: from the first character of one text to the last
/// character of another, as the grid shows them.
#[cfg(feature = "ui-capture")]
#[derive(Clone, Copy, Debug)]
struct CaptureSelection {
    from: &'static str,
    to: &'static str,
    kind: SelectionType,
}

/// The first and the last grid column `needle` covers in a row of cells, if the row
/// holds it. A wide character covers two columns and has one cell.
#[cfg(feature = "ui-capture")]
fn columns_of(cells: &[RenderedCell], needle: &str) -> Option<(usize, usize)> {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(cells.len());
    let mut column = 0;
    for cell in cells {
        let width = cell.width.max(1);
        starts.push((text.len(), column, width));
        text.push_str(&cell.text);
        column += width;
    }
    let at = text.rfind(needle)?;
    let end = at + needle.len();
    let first = starts.iter().rev().find(|(offset, ..)| *offset <= at)?;
    let last = starts.iter().rev().find(|(offset, ..)| *offset < end)?;
    Some((first.1, last.1 + last.2 - 1))
}

/// How far the pointer moves, in pixels, before a press becomes a drag.
///
/// Below it, and inside the cell the button went down on, the press is a click: a
/// hand that shakes while clicking must not select, and copy, one character.
pub const SELECTION_DRAG_THRESHOLD: f32 = 3.0;

/// How often a drag held past the top or bottom edge scrolls the view.
pub const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// The most lines one auto-scroll step moves, however far past the edge the
/// pointer is.
pub const SELECTION_AUTOSCROLL_MAX_LINES: usize = 5;

/// A left-button selection gesture the terminal handles itself.
///
/// The press only records where it was. The selection begins once the pointer
/// has moved more than [`SELECTION_DRAG_THRESHOLD`] or crossed into another cell;
/// a double or triple click, and a Shift+click, begin it at once. That is what
/// separates a click from a selection: a click selected nothing, and copying would
/// replace the user's clipboard with a character they never chose.
#[derive(Clone, Copy, Debug)]
struct SelectionDrag {
    /// Where the button went down, in window pixels.
    origin: gpui::Point<Pixels>,
    /// The cell the button went down on and the half of it the pointer was in.
    anchor: ((usize, usize), Side),
    /// The grid line the button went down on, stable across streaming output.
    anchor_line: Option<LineAnchor>,
    /// What the drag selects: characters, a block (Alt/Option), words or lines.
    kind: SelectionType,
    /// The selection has begun.
    started: bool,
    /// Where the pointer last was, which auto-scroll keeps extending to.
    pointer: gpui::Point<Pixels>,
}

/// Height of the status line under the grid.
pub const SHELL_BAR_HEIGHT: f32 = 28.0;

/// What the bar's last layout measured: its own width and the width of the session's
/// middle group, which decides which of the session's controls give way.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct BarRoom {
    width: Option<Pixels>,
    /// The natural width of the session's controls that give way in a narrow bar.
    controls: Pixels,
    /// The width of the session's controls that always stay on the bar: its menu.
    pinned: Pixels,
    /// How many of the controls that give way wrapped off the bar's one line.
    wrapped: usize,
    /// How many controls give way.
    count: usize,
    /// Where the first of them ends; it never wraps, so a group narrower than it cuts
    /// it instead.
    first_right: Option<Pixels>,
    /// Whether the group is narrower than its first control, so none of them is whole
    /// on the bar and the group is not drawn.
    clipped: bool,
    /// How many of the controls that give way, counted from the end, were left off
    /// the bar at its last layout (R3, R9).
    hidden: usize,
}

/// What closes a menu the window drew.
pub type CloseMenu = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

/// How a command in a menu of the pane's tools closes the menu it is in (ADR-7).
#[derive(Clone)]
pub enum MenuCloser {
    /// The pane's own "more" menu.
    Own,
    /// A menu the window drew - the session's menu - closed by the window.
    Window(CloseMenu),
}

impl MenuCloser {
    /// Closes the menu the command was chosen in.
    fn close(&self, pane: &mut TerminalPane, window: &mut Window, cx: &mut App) {
        match self {
            Self::Own => pane.more_menu = None,
            Self::Window(close) => close(window, cx),
        }
    }
}

/// The session's half of the bar under a terminal, handed in by the window for one
/// draw (R3).
pub struct SessionSlot {
    /// Where the session is: its mark, its target and its chips.
    pub left: gpui::AnyElement,
    /// The session's controls, in their order of importance: in a bar too narrow for
    /// all of them the last ones give way first, each one whole.
    pub controls: Vec<gpui::AnyElement>,
    /// The controls that never give way: the session's menu, which reaches every
    /// control the bar had no room for.
    pub pinned: Vec<gpui::AnyElement>,
}

/// Width of the caret in logical pixels.
const CARET_WIDTH: f32 = 2.0;

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

/// What a serial line left behind when its pane stopped driving it (R21).
///
/// A serial close is the one close whose result a person has to read: the driver may
/// still hold the port, and frames the user typed may never have left the queue. The
/// pane keeps the sentence on its strip, and the window shows the same words when the
/// pane is dropped in the same breath - a tab that disappears with the reason inside it
/// tells nobody anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialClose {
    /// The device the sentence is about.
    pub device: String,
    /// What remained: a port still held, or frames that were never sent.
    pub note: String,
}

/// What happened to the shell behind a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneState {
    /// The shell is running.
    Live,
    /// The shell could not be started.
    ///
    /// Kept apart from [Self::Lost] because the next step differs: a program that never
    /// ran needs its path or its permission fixed, while a session that was live and
    /// stopped can be reconnected as it is (R17).
    Failed(SessionFailure),
    /// The shell was live and stopped: a lost channel, a closed pipe, a dead peer.
    ///
    /// The reason still says what the layer reported; the variant says that the program
    /// did run, so nothing on screen may call this a launch failure.
    Lost(SessionFailure),
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
            Self::Failed(failure) | Self::Lost(failure) => Some(failure.clone()),
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
            Self::Lost(_) => strings.terminal_disconnected.to_owned(),
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
    /// An open serial device (R21).
    ///
    /// A serial port is a byte stream in both directions and nothing more, so it is the
    /// same grid, selection, scrollback and copy a local shell uses: only the byte
    /// source differs. It is not a remote shell and it has no window size, which is why
    /// a resize is not sent anywhere.
    Serial(SerialLink),
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
    /// Which operating system the server said it runs; the window keeps it for the
    /// host's tile.
    Platform(String),
}

/// Parse adjacent reads together; control events keep their position in the stream.
fn coalesce_output(events: Vec<SourceEvent>) -> Vec<SourceEvent> {
    let mut merged = Vec::new();
    for event in events {
        match event {
            SourceEvent::Data(bytes) => {
                if let Some(SourceEvent::Data(previous)) = merged.last_mut() {
                    previous.extend_from_slice(&bytes);
                } else {
                    merged.push(SourceEvent::Data(bytes));
                }
            }
            control => merged.push(control),
        }
    }
    merged
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
            RemoteEvent::Platform(platform) => Some(Self::Platform(platform)),
            RemoteEvent::Failed(error) => Some(Self::Lost(error.to_string())),
        }
    }
}

impl PaneSource {
    fn write(&mut self, bytes: &[u8], locale: Locale) -> Result<bool, String> {
        match self {
            Self::Local(session) => session.try_write(bytes).map_err(|error| error.to_string()),
            Self::Remote(session) => session.try_write(bytes),
            Self::Chain(session) => session.try_write(bytes),
            Self::Serial(link) => link.write_for(bytes, locale),
        }
    }

    fn resize(&mut self, grid: GridSize) -> Result<(), String> {
        match self {
            Self::Local(session) => session.resize(grid),
            Self::Remote(session) => session.resize(pty_size(grid)),
            Self::Chain(session) => session.resize(pty_size(grid)),
            // A serial line has no window: the device sends and receives bytes and knows
            // nothing about rows or columns. Saying "resized" would be a claim about a
            // device that never heard of it, so nothing is sent and nothing is reported.
            Self::Serial(_) => Ok(()),
        }
    }

    fn drain(&mut self, locale: Locale) -> Vec<SourceEvent> {
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
            Self::Serial(link) => link
                .poll_for(locale)
                .into_iter()
                .filter_map(|event| match event {
                    shellune_serial::SerialEvent::Received(bytes) => Some(SourceEvent::Data(bytes)),
                    // A device that went away is a session that was live and is not any
                    // more; the sentence the platform gave is kept.
                    shellune_serial::SerialEvent::Disconnected { error } => {
                        Some(SourceEvent::Lost(describe_for(&error, locale)))
                    }
                    // A release states no new state of the *terminal*: a close started
                    // here has already moved the pane, and a device that failed reported
                    // Disconnected first. The rest are facts about the line that
                    // `SerialLink` keeps for the bar - a refused send, a frame the driver
                    // cut short, bytes it dropped - and not changes of state.
                    shellune_serial::SerialEvent::Closed
                    | shellune_serial::SerialEvent::Sent { .. }
                    | shellune_serial::SerialEvent::SendIncomplete { .. }
                    | shellune_serial::SerialEvent::ReceiveOverflow { .. } => None,
                })
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
                    ChainEvent::Platform(platform) => Some(SourceEvent::Platform(platform)),
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
            // A serial port is released through `SerialLink::close`, which reports what
            // the close achieved: the pane states a port it could not release instead of
            // pretending the device is free.
            Self::Serial(link) => {
                let _ = link.close();
            }
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
    /// A serial device (R21).
    Serial {
        /// The port the session holds, as the platform spells it.
        device: String,
    },
}

impl PaneOrigin {
    /// What the pane calls the thing it runs.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Local(shell) => shell.name(),
            Self::Remote { target } => target,
            Self::Serial { device } => device,
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
            // The device is not a server and not a program: the sentence names the port
            // and keeps the platform reason, so a disconnected session says which device
            // went away and why instead of borrowing a network or a shell word (R21).
            Self::Serial { device } => SessionFailure::SerialDeviceFailed {
                port: device.clone(),
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
    /// The server said which operating system it runs.
    Platform(String),
    /// Open the snippets / shortcut commands manager for this terminal.
    OpenSnippets,
    /// Split this terminal's pane along `axis`.
    Split(shellune_core::SplitAxis),
    /// Start writing this terminal's output to a file the user picks (R3).
    StartLog(LogFormat),
    /// Stop the running log; the window says where it was saved (R3).
    StopLog,
    /// The log stopped because writing failed; the terminal goes on (R3).
    LogFailed {
        /// The file it was writing.
        path: std::path::PathBuf,
        /// Why writing failed.
        reason: String,
    },
    /// An OSC 8 link was not opened, and why (R1).
    LinkRefused(LinkRefusal),
    /// The user changed the serial newline mode; persist it on the saved record.
    SerialFormatChanged(NewlineFormat),
    /// Reconnect this serial pane in-place, optionally switching to another detected port.
    SerialReconnect(Option<String>),
}

impl gpui::EventEmitter<PaneRequest> for TerminalPane {}

/// How long the cursor stays lit, and then dark, in one blink.
const CURSOR_BLINK: Duration = Duration::from_millis(530);

/// The focused cursor's blink.
///
/// A blinking cursor is how a terminal says "the keys land here" at a glance among
/// several panes and windows. It blinks only while the pane holds the keyboard, stays
/// lit while the user types or the program writes - a cursor that vanishes mid-word
/// is hard to follow - and does not blink at all under reduced motion.
#[derive(Default)]
struct CursorBlink {
    /// Whether the cursor is in the dark half of its blink.
    dark: bool,
    /// Whether the timer that drives the blink is running.
    running: bool,
    /// Whether the pane held the keyboard when it was last drawn.
    focused: bool,
    /// Until when the cursor stays lit after a key or new output.
    lit_until: Option<std::time::Instant>,
}

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

/// A desktop notification a terminal asked the window to consider (R2).
///
/// Kept apart from [`TerminalAttention`], which never carries text: these do, and
/// only reach the desktop when the user has notifications on and the terminal is not
/// in front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneNotice {
    /// A program asked with OSC 9 or OSC 777.
    Program(ProgramNotice),
    /// A command that ran for a while ended.
    Finished(FinishedCommand),
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

#[derive(Default)]
struct OutputRepaint {
    deadline: Option<std::time::Instant>,
    bytes: usize,
    pending: bool,
}

pub struct TerminalPane {
    /// Which session process this pane shows.
    ///
    /// Every event a source produces is checked against it before it is applied, and
    /// the window passes events with the tag it drained them under, so a batch that
    /// belongs to a process this pane does not show is dropped instead of being drawn
    /// into the wrong terminal.
    tag: EventTag,
    /// The session's own half of the bar under the grid, handed in by the window for
    /// the frame being drawn: where the session is and what can be done to it.
    session_slot: std::cell::RefCell<Option<SessionSlot>>,
    /// The banner the window puts above the grid for the next draw: why the session
    /// ended, or that it is coming back (ADR-6), and the first of its controls the
    /// keyboard reaches. Taken like the session's half of the bar.
    banner_slot: std::cell::RefCell<Option<(Vec<gpui::AnyElement>, Option<FocusHandle>)>>,
    /// The first control of the banner drawn last, which Tab reaches from a terminal
    /// that no longer takes input.
    banner_focus: std::cell::RefCell<Option<FocusHandle>>,
    /// How wide the bar was at its last layout and how much of it the session's own
    /// middle group took: what decides which of its controls give way (R3).
    bar_room: std::cell::Cell<BarRoom>,
    /// Where the bar's "more" menu is open, when it is.
    more_menu: Option<gpui::Point<Pixels>>,
    /// Whether the bar carried the session's half at its last draw.
    bar_has_slot: std::cell::Cell<bool>,
    /// Whether the window is in focus mode, hiding the regular bottom bar.
    focus_mode: std::cell::Cell<bool>,
    core: TerminalCore,
    source: Option<PaneSource>,
    /// What the pane is running: a program here, or a host over SSH.
    origin: PaneOrigin,
    /// The file this terminal's output is being written to, while it is (R3).
    log: Option<SessionLog>,
    /// Images converted for drawing, by the terminal's image id, so an image is
    /// converted once and not on every frame (R4).
    image_cache:
        std::cell::RefCell<std::collections::HashMap<u64, std::sync::Arc<gpui::RenderImage>>>,
    /// How many images the last frame drew, for tests.
    #[cfg(test)]
    painted_images: std::cell::Cell<usize>,
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
    /// The left-button selection gesture, from the press to the release.
    drag: Option<SelectionDrag>,
    /// A timer is scrolling the view while a drag holds the pointer past the top
    /// or bottom edge of the grid.
    autoscrolling: bool,
    /// Whether the pointer is dragging the scrollback scrollbar track.
    scrollbar_drag: bool,
    /// Where the focused cursor is in its blink.
    cursor_blink: CursorBlink,
    /// The button a program is being told is held, while it asked for the mouse.
    mouse_held: Option<TerminalMouseButton>,
    /// The cell the last motion report named, so moving inside one cell does not
    /// report it again.
    last_report_cell: Option<(usize, usize)>,
    /// Where the right-click menu is open.
    menu: Option<gpui::Point<Pixels>>,
    /// Openable `http(s)://` URL under the pointer when the right-click menu opened.
    menu_link: Option<String>,
    /// Openable `http(s)://` URL currently hovered while the primary modifier is held.
    hovered_link: Option<DetectedLink>,
    /// Whether the modifier that opens the hovered link is held, so a click opens it.
    link_armed: bool,
    /// Last pointer position over the terminal viewport, for modifier-change updates.
    last_hover_position: Option<gpui::Point<Pixels>>,
    /// The modifiers held at the last pointer or modifier event, for re-checking the
    /// hovered link when the grid moves under a still pointer (R1).
    last_hover_modifiers: gpui::Modifiers,
    #[cfg(test)]
    opened_urls: Vec<String>,
    marked: Option<String>,
    last_copy: Option<String>,
    /// The selection a capture asked for, put back whenever the grid changes size.
    #[cfg(feature = "ui-capture")]
    capture_selection: Option<CaptureSelection>,
    locale: Locale,
    poll_task: Option<gpui::Task<()>>,
    /// A multi-line or high-risk paste that has not been confirmed.
    ///
    /// It is not encoded and not written until [`TerminalPane::confirm_paste`].
    /// Cancel drops it. Program keys are not answers to this prompt.
    pending_paste: Option<PendingPaste>,
    paste_waiting: std::sync::Arc<std::sync::atomic::AtomicBool>,
    search_open: bool,
    /// When set, typed text edits the search query instead of the shell.
    ///
    /// Ctrl+C, Ctrl+L and the program's own arrow keys were never taken by the
    /// search; Escape belongs to the find bar only while it holds the keyboard, and
    /// then it closes the bar and gives the terminal back (R1, R4).
    search_focused: bool,
    search_query: String,
    /// Byte offset of the editing caret inside [`Self::search_query`].
    search_cursor: usize,
    /// Active byte selection inside [`Self::search_query`] (e.g. `Ctrl/Cmd+A`).
    search_selection: Option<Range<usize>>,
    /// Measured screen bounds of the search query text container for click caret placement.
    search_text_bounds: std::cell::Cell<Option<Bounds<Pixels>>>,
    /// Whether new output arrived on the primary screen while parked in scrollback.
    unseen_output: bool,
    /// Why the query could not be compiled, in the expression engine's words.
    ///
    /// An unusable expression is its own answer: it is never drawn as "no matches".
    search_error: Option<String>,
    input_busy: bool,
    output_repaint: OutputRepaint,
    output_search_at: Option<std::time::Instant>,
    output_search_pending: bool,
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
    /// Lines a trackpad has scrolled that did not add up to a whole line yet.
    ///
    /// A trackpad reports most of a gesture in deltas smaller than one line.
    /// Moving a line for each of them turns a gentle flick into a jump, and makes
    /// a stray sub-pixel event — the ones a gesture ends with as the fingers
    /// settle — a whole line of its own, which reads as the view scrolling by
    /// itself. The fraction waits until it adds up.
    scroll_fraction: f32,
    /// A phased trackpad gesture stays at the bottom once it reaches it. Its
    /// settling/momentum events can reverse direction; only a new gesture may
    /// leave the bottom again. Unphased mouse wheels do not use this latch.
    scroll_gesture_active: bool,
    scroll_gesture_pinned: bool,
    block_filter: BlockFilter,
    /// The shortcuts the pane's own controls answer, as the bindings in force show
    /// them (R5). The view keeps this in step with the settings page.
    shortcuts: TerminalShortcuts,
    /// Background notices the window has not collected yet (R7).
    ///
    /// They are taken by the window, which knows which tab is on screen; this pane only
    /// remembers that something happened, never what it was.
    attention: TerminalAttention,
    /// Desktop notifications waiting for the window to decide about them (R2).
    system_notices: Vec<PaneNotice>,
    /// What X11 forwarding of this session reported last (R11).
    ///
    /// Taken by the window like the background notices are: the pane only holds
    /// what arrived since the last look.
    x11: Option<X11Status>,
    /// The working directory the shell last stated with OSC 7 (R15).
    ///
    /// Taken by the window like the notices are: the pane reports that the shell
    /// said where it is, and the window - which knows whether a file view is open
    /// for this session - decides whether a directory follows. A pane whose shell
    /// has said nothing keeps `None`, which is why nothing is ever assumed.
    directory: Option<String>,
    /// What the shell could not be given when it started (R14, R15).
    ///
    /// Read once from the session and kept, because the notice belongs to this pane
    /// for as long as it is on screen: the environment of a running shell cannot
    /// change, and a note that disappeared would leave the user with an unexplained
    /// locale or an unexplained missing program.
    environment_notes: Vec<ShellEnvironmentNote>,
    /// What a serial pane remembers beside its session (R21).
    ///
    /// Kept in the pane rather than in [`SerialLink`] because it outlives the port: the
    /// device, its settings, the two send habits, the port list and the one line about
    /// the line are what a pane whose device could not be opened still has to show.
    serial: Option<SerialPane>,
}

/// What a terminal's copy, paste and search controls answer, written the way a
/// person reads a shortcut. `None` means the action is unbound, so the tooltip and the
/// menu say nothing rather than naming a key the window would not take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalShortcuts {
    /// The search action, `Ctrl+Shift+F` or `⌘F` by default.
    pub search: Option<String>,
    /// The copy action, `Ctrl+C` or `⌘C` by default. With nothing selected the
    /// combination stays the program's own key (an interrupt on Ctrl+C).
    pub copy: Option<String>,
    /// The paste action, `Ctrl+V` or `⌘V` by default.
    pub paste: Option<String>,
    /// Split pane right.
    pub split_right: Option<String>,
    /// Split pane down.
    pub split_down: Option<String>,
}

impl TerminalShortcuts {
    /// The labels of the bindings in force.
    #[must_use]
    pub fn of(keymap: &crate::app::keymap::Keymap) -> Self {
        Self {
            search: keymap.label("terminal.search"),
            copy: keymap.label("terminal.copy"),
            paste: keymap.label("terminal.paste"),
            split_right: keymap.label("pane.split-right"),
            split_down: keymap.label("pane.split-down"),
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

/// What a pane's reader waits on between drains.
enum Ready {
    Ssh(shellune_ssh::OutputReady),
    #[cfg_attr(test, allow(dead_code))]
    Local(shellune_terminal::LocalOutputReady),
}

impl Ready {
    async fn wait(&self) {
        match self {
            Self::Ssh(ready) => ready.wait().await,
            Self::Local(ready) => ready.wait().await,
        }
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
    /// Wraps a local process after its PTY has opened on the background executor.
    pub fn from_local(
        shell: LocalShell,
        session: LocalSession,
        tag: EventTag,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_size = sanitize_font_size(font_size);
        let grid = grid.clamped();
        let core = new_core(grid, shellune_terminal::DEFAULT_SCROLLBACK);
        let environment_notes = session.environment().notes().to_vec();
        let mut pane = Self {
            tag,
            core,
            source: Some(PaneSource::Local(session)),
            origin: PaneOrigin::Local(shell),
            state: PaneState::Live,
            #[cfg(test)]
            sent: Vec::new(),
            font_size,
            cell: CellSize::for_font_size(font_size),
            grid,
            viewport: None,
            focus: cx.focus_handle(),
            drag: None,
            autoscrolling: false,
            scrollbar_drag: false,
            cursor_blink: CursorBlink::default(),
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            menu_link: None,
            hovered_link: None,
            link_armed: false,
            last_hover_position: None,
            last_hover_modifiers: gpui::Modifiers::default(),
            #[cfg(test)]
            opened_urls: Vec::new(),
            marked: None,
            last_copy: None,
            #[cfg(feature = "ui-capture")]
            capture_selection: None,
            locale,
            poll_task: None,
            pending_paste: None,
            paste_waiting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_open: false,
            session_slot: std::cell::RefCell::new(None),
            banner_slot: std::cell::RefCell::new(None),
            banner_focus: std::cell::RefCell::new(None),
            bar_room: std::cell::Cell::new(BarRoom::default()),
            more_menu: None,
            bar_has_slot: std::cell::Cell::new(false),
            focus_mode: std::cell::Cell::new(false),
            search_focused: false,
            search_query: String::new(),
            search_cursor: 0,
            search_selection: None,
            search_text_bounds: std::cell::Cell::new(None),
            unseen_output: false,
            search_error: None,
            input_busy: false,
            output_repaint: OutputRepaint::default(),
            output_search_at: None,
            output_search_pending: false,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            scroll_fraction: 0.0,
            scroll_gesture_active: false,
            scroll_gesture_pinned: false,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            system_notices: Vec::new(),
            log: None,
            image_cache: std::cell::RefCell::default(),
            #[cfg(test)]
            painted_images: std::cell::Cell::new(0),
            x11: None,
            directory: None,
            environment_notes,
            serial: None,
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
        let core = new_core(grid, shellune_terminal::DEFAULT_SCROLLBACK);
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
            drag: None,
            autoscrolling: false,
            scrollbar_drag: false,
            cursor_blink: CursorBlink::default(),
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            menu_link: None,
            hovered_link: None,
            link_armed: false,
            last_hover_position: None,
            last_hover_modifiers: gpui::Modifiers::default(),
            #[cfg(test)]
            opened_urls: Vec::new(),
            marked: None,
            last_copy: None,
            #[cfg(feature = "ui-capture")]
            capture_selection: None,
            locale,
            poll_task: None,
            pending_paste: None,
            paste_waiting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_open: false,
            session_slot: std::cell::RefCell::new(None),
            banner_slot: std::cell::RefCell::new(None),
            banner_focus: std::cell::RefCell::new(None),
            bar_room: std::cell::Cell::new(BarRoom::default()),
            more_menu: None,
            bar_has_slot: std::cell::Cell::new(false),
            focus_mode: std::cell::Cell::new(false),
            search_focused: false,
            search_query: String::new(),
            search_cursor: 0,
            search_selection: None,
            search_text_bounds: std::cell::Cell::new(None),
            unseen_output: false,
            search_error: None,
            input_busy: false,
            output_repaint: OutputRepaint::default(),
            output_search_at: None,
            output_search_pending: false,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            scroll_fraction: 0.0,
            scroll_gesture_active: false,
            scroll_gesture_pinned: false,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            system_notices: Vec::new(),
            log: None,
            image_cache: std::cell::RefCell::default(),
            #[cfg(test)]
            painted_images: std::cell::Cell::new(0),
            x11: None,
            directory: None,
            environment_notes: Vec::new(),
            serial: None,
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
        let core = new_core(grid, shellune_terminal::DEFAULT_SCROLLBACK);
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
            drag: None,
            autoscrolling: false,
            scrollbar_drag: false,
            cursor_blink: CursorBlink::default(),
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            menu_link: None,
            hovered_link: None,
            link_armed: false,
            last_hover_position: None,
            last_hover_modifiers: gpui::Modifiers::default(),
            #[cfg(test)]
            opened_urls: Vec::new(),
            marked: None,
            last_copy: None,
            #[cfg(feature = "ui-capture")]
            capture_selection: None,
            locale,
            poll_task: None,
            pending_paste: None,
            paste_waiting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_open: false,
            session_slot: std::cell::RefCell::new(None),
            banner_slot: std::cell::RefCell::new(None),
            banner_focus: std::cell::RefCell::new(None),
            bar_room: std::cell::Cell::new(BarRoom::default()),
            more_menu: None,
            bar_has_slot: std::cell::Cell::new(false),
            focus_mode: std::cell::Cell::new(false),
            search_focused: false,
            search_query: String::new(),
            search_cursor: 0,
            search_selection: None,
            search_text_bounds: std::cell::Cell::new(None),
            unseen_output: false,
            search_error: None,
            input_busy: false,
            output_repaint: OutputRepaint::default(),
            output_search_at: None,
            output_search_pending: false,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            scroll_fraction: 0.0,
            scroll_gesture_active: false,
            scroll_gesture_pinned: false,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            system_notices: Vec::new(),
            log: None,
            image_cache: std::cell::RefCell::default(),
            #[cfg(test)]
            painted_images: std::cell::Cell::new(0),
            x11: None,
            directory: None,
            environment_notes: Vec::new(),
            serial: None,
        };
        pane.start_polling(cx);
        pane
    }

    /// Opens the serial device a saved record names and wraps it in a pane (R21).
    ///
    /// A device that will not open is still a pane, and that is deliberate: the reason
    /// the platform gave, the settings that were asked for and the port list are what
    /// the user needs, and a tab that vanished would leave them with nothing to act on.
    /// The state is [`PaneState::Failed`], never [`PaneState::Live`], so nothing here is
    /// drawn as a connected terminal.
    pub fn from_serial(
        target: &SerialTarget,
        opened: Result<SerialLink, String>,
        tag: EventTag,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::serial_skeleton(
            target.device(),
            target.config().summary(),
            target.local_echo(),
            target.newline(),
            tag,
            grid,
            font_size,
            locale,
            cx,
        );
        match opened {
            Ok(link) => {
                pane.source = Some(PaneSource::Serial(link));
                pane.state = PaneState::Live;
            }
            Err(reason) => {
                // The list is asked at the moment of failure, so the card can say what
                // *is* attached while the record names what is not. It is asked once and
                // kept until the refresh control is used.
                if let Some(serial) = pane.serial.as_mut() {
                    serial.refresh_ports();
                    serial.set_note_with_tone(Some(reason.clone()), Tone::Danger);
                }
                pane.state = PaneState::Failed(SessionFailure::ConnectionFailed { reason });
            }
        }
        pane.start_polling(cx);
        pane
    }

    /// A serial pane whose session is already open.
    ///
    /// The tests drive the real path with a real [`SerialLink`] - reader, writer,
    /// bounded queue, close - over an in-memory port, because this machine has no
    /// serial device and no virtual pair. Nothing in the application opens a port this
    /// way: a saved record always goes through [`SerialLink::open`].
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn start_serial_with(
        link: SerialLink,
        device: &str,
        settings: &str,
        local_echo: bool,
        tag: EventTag,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::serial_skeleton(
            device,
            settings.to_owned(),
            local_echo,
            NewlineFormat::AsTyped,
            tag,
            grid,
            font_size,
            locale,
            cx,
        );
        pane.source = Some(PaneSource::Serial(link));
        pane.state = PaneState::Live;
        pane.start_polling(cx);
        pane
    }

    /// The empty shell both serial constructors start from.
    #[allow(clippy::too_many_arguments)]
    fn serial_skeleton(
        device: &str,
        settings: String,
        local_echo: bool,
        newline: NewlineFormat,
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
            core: new_core(grid, shellune_terminal::DEFAULT_SCROLLBACK),
            source: None,
            origin: PaneOrigin::Serial {
                device: device.to_owned(),
            },
            // Nothing is live until a link exists: the constructors above either set a
            // source and this to Live, or leave both and report the reason.
            state: PaneState::Failed(SessionFailure::ConnectionFailed {
                reason: "the device has not been opened".to_owned(),
            }),
            #[cfg(test)]
            sent: Vec::new(),
            font_size,
            cell: CellSize::for_font_size(font_size),
            grid,
            viewport: None,
            focus: cx.focus_handle(),
            drag: None,
            autoscrolling: false,
            scrollbar_drag: false,
            cursor_blink: CursorBlink::default(),
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            menu_link: None,
            hovered_link: None,
            link_armed: false,
            last_hover_position: None,
            last_hover_modifiers: gpui::Modifiers::default(),
            #[cfg(test)]
            opened_urls: Vec::new(),
            marked: None,
            last_copy: None,
            #[cfg(feature = "ui-capture")]
            capture_selection: None,
            locale,
            poll_task: None,
            pending_paste: None,
            paste_waiting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_open: false,
            session_slot: std::cell::RefCell::new(None),
            banner_slot: std::cell::RefCell::new(None),
            banner_focus: std::cell::RefCell::new(None),
            bar_room: std::cell::Cell::new(BarRoom::default()),
            more_menu: None,
            bar_has_slot: std::cell::Cell::new(false),
            focus_mode: std::cell::Cell::new(false),
            search_focused: false,
            search_query: String::new(),
            search_cursor: 0,
            search_selection: None,
            search_text_bounds: std::cell::Cell::new(None),
            unseen_output: false,
            search_error: None,
            input_busy: false,
            output_repaint: OutputRepaint::default(),
            output_search_at: None,
            output_search_pending: false,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            system_notices: Vec::new(),
            log: None,
            image_cache: std::cell::RefCell::default(),
            #[cfg(test)]
            painted_images: std::cell::Cell::new(0),
            x11: None,
            directory: None,
            environment_notes: Vec::new(),
            serial: Some(SerialPane::with_format(
                device, settings, local_echo, newline,
            )),
            scroll_fraction: 0.0,
            scroll_gesture_active: false,
            scroll_gesture_pinned: false,
        }
    }

    /// A serial pane with no session behind it, for the capture probe (R21).
    ///
    /// The same pane the application draws for a live port - the same strip, the same
    /// bar, the same grid - with no device: the bytes the caller supplies stand in for
    /// what a device would have sent, and [`PaneState`] says which state is being
    /// captured. Nothing here opens a port, so a shot taken with it proves what the
    /// interface looks like and nothing about a device.
    #[cfg(feature = "ui-capture")]
    #[allow(clippy::too_many_arguments)]
    pub fn serial_without_device(
        device: &str,
        settings: &str,
        local_echo: bool,
        state: PaneState,
        ports: PortScan,
        note: Option<String>,
        content: &[u8],
        tag: EventTag,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::serial_skeleton(
            device,
            settings.to_owned(),
            local_echo,
            NewlineFormat::AsTyped,
            tag,
            grid,
            font_size,
            locale,
            cx,
        );
        pane.state = state;
        if let Some(serial) = pane.serial.as_mut() {
            serial.show_ports(ports);
            serial.set_note_with_tone(note, Tone::Warning);
        }
        if !content.is_empty() {
            let was_alternate = pane.alternate_screen();
            let effects = pane.core.feed(content);
            pane.apply_effects(effects, cx);
            pane.recount_after_output(was_alternate);
        }
        pane
    }
    /// Shuts the source down because the user closed it.
    ///
    /// The last screen stays on display: dropping the grid on close would hide what
    /// was on it, and the pane reports that it is no longer live either way.
    ///
    /// What a serial close left behind is returned as well as kept: the note belongs on
    /// the pane for as long as it is drawn, and the window needs the same sentence for
    /// the moment the pane is dropped - a tab that disappears with the reason still
    /// inside it says nothing to anyone (R21).
    pub fn close(&mut self, cx: &mut Context<Self>) -> Option<SerialClose> {
        // The log is written to the end of what arrived before the source goes (R3).
        drop(self.log.take());
        // Image textures live in the window's atlas, not in the pane: they are let
        // go with it, or a window that opens and closes terminals keeps them (R4).
        for (_, image) in self.image_cache.get_mut().drain() {
            cx.drop_image(image, None);
        }
        let mut closed = None;
        match self.source.as_mut() {
            Some(PaneSource::Serial(link)) => {
                let report = link.close();
                closed = self.keep_close_note(&report);
            }
            Some(source) => source.close(),
            None => {}
        }
        self.source = None;
        self.poll_task = None;
        if self.state.accepts_input() {
            self.state = PaneState::Exited {
                report: ExitReport::Clean,
            };
        }
        cx.notify();
        closed
    }

    /// Releases the port of a serial pane whose session is already over (R17, R21).
    ///
    /// A device that went away, a read that failed and a close all end the connection,
    /// and the writer thread together with the queue of bytes waiting for the device
    /// must not outlive it: until the port is released the driver keeps the line open,
    /// so the next open of the same device fails for a reason nothing on screen explains.
    ///
    /// The pane stays exactly as it is - it still names the device and the failure - and
    /// the sentence about what remained is the one a close states.
    pub fn release_serial(&mut self, cx: &mut Context<Self>) -> Option<SerialClose> {
        let Some(PaneSource::Serial(link)) = self.source.as_mut() else {
            return None;
        };
        drop(self.log.take());
        let report = link.close_for(self.locale);
        self.source = None;
        self.poll_task = None;
        let closed = self.keep_close_note(&report);
        cx.notify();
        closed
    }

    /// Releases the serial port without closing this tab, leaving the pane ready to reconnect.
    pub fn disconnect_serial(&mut self, cx: &mut Context<Self>) {
        if self.serial.is_none() || !self.state.accepts_input() {
            return;
        }
        let _ = self.release_serial(cx);
        if let Some(serial) = self.serial.as_mut() {
            serial.refresh_ports();
        }
        self.state = PaneState::Exited {
            report: ExitReport::Clean,
        };
        cx.notify();
    }

    /// Reconnects this serial pane in-place with a newly opened link (or records the open failure).
    pub fn reconnect_serial_with(
        &mut self,
        device: &str,
        opened: Result<SerialLink, String>,
        cx: &mut Context<Self>,
    ) {
        if self.serial.is_none() {
            return;
        }
        let _ = self.release_serial(cx);
        self.origin = PaneOrigin::Serial {
            device: device.to_owned(),
        };
        if let Some(serial) = self.serial.as_mut() {
            serial.set_device(device);
        }
        match opened {
            Ok(link) => {
                self.source = Some(PaneSource::Serial(link));
                self.state = PaneState::Live;
                if let Some(serial) = self.serial.as_mut() {
                    serial.clear_note();
                }
                self.start_polling(cx);
            }
            Err(reason) => {
                if let Some(serial) = self.serial.as_mut() {
                    serial.refresh_ports();
                    serial.set_note_with_tone(Some(reason.clone()), Tone::Danger);
                }
                self.state = PaneState::Failed(SessionFailure::ConnectionFailed { reason });
            }
        }
        cx.notify();
    }

    /// Sends a serial BREAK signal on the active port.
    pub fn send_serial_break(&mut self, cx: &mut Context<Self>) {
        let result = match self.source.as_ref() {
            Some(PaneSource::Serial(link)) => link.send_break(self.locale),
            _ => return,
        };
        if let Some(serial) = self.serial.as_mut() {
            match result {
                Ok(()) => {
                    serial.say_with_tone(self.locale.strings().serial_break_sent, Tone::Positive)
                }
                Err(reason) => serial.say_with_tone(reason, Tone::Danger),
            }
        }
        cx.notify();
    }

    /// Dismisses the current serial status bar note.
    pub fn dismiss_serial_note(&mut self, cx: &mut Context<Self>) {
        if let Some(serial) = self.serial.as_mut()
            && serial.note().is_some()
        {
            serial.clear_note();
            cx.notify();
        }
    }

    /// Keeps what a close reported, and hands it to the window as well.
    ///
    /// A port the driver did not release is stated as such, and so are the frames that
    /// never left the queue: a session that said nothing would let the next open of the
    /// same port fail for a reason no one can see, or lose a frame the user typed with
    /// no word about it (R17, R21). An older note is replaced, because only one line is
    /// drawn and the newest fact about the line is the true one.
    fn keep_close_note(&mut self, report: &CloseReport) -> Option<SerialClose> {
        let note = report.note.as_ref()?;
        let held = self.locale.strings().serial_still_held;
        let sentence = if report.released {
            note.clone()
        } else {
            format!("{held}: {note}")
        };
        if let Some(serial) = self.serial.as_mut() {
            let tone = if report.released {
                Tone::Warning
            } else {
                Tone::Danger
            };
            serial.set_note_with_tone(Some(sentence.clone()), tone);
        }
        Some(SerialClose {
            device: self.serial.as_ref().map_or_else(
                || self.origin.label().to_owned(),
                |serial| serial.device().to_owned(),
            ),
            note: sentence,
        })
    }

    /// A pane with a terminal but no process.
    ///
    /// Tests use this to drive the grid deterministically; the application always
    /// goes through [`TerminalPane::start`]. The capture tool uses the same pane with
    /// bytes it supplies, so a screenshot can show terminal text without a PTY (R2).
    #[cfg(any(test, feature = "ui-capture"))]
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
            core: new_core(grid, 1000),
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
            drag: None,
            autoscrolling: false,
            scrollbar_drag: false,
            cursor_blink: CursorBlink::default(),
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            menu_link: None,
            hovered_link: None,
            link_armed: false,
            last_hover_position: None,
            last_hover_modifiers: gpui::Modifiers::default(),
            #[cfg(test)]
            opened_urls: Vec::new(),
            marked: None,
            last_copy: None,
            #[cfg(feature = "ui-capture")]
            capture_selection: None,
            locale,
            poll_task: None,
            pending_paste: None,
            paste_waiting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_open: false,
            session_slot: std::cell::RefCell::new(None),
            banner_slot: std::cell::RefCell::new(None),
            banner_focus: std::cell::RefCell::new(None),
            bar_room: std::cell::Cell::new(BarRoom::default()),
            more_menu: None,
            bar_has_slot: std::cell::Cell::new(false),
            focus_mode: std::cell::Cell::new(false),
            search_focused: false,
            search_query: String::new(),
            search_cursor: 0,
            search_selection: None,
            search_text_bounds: std::cell::Cell::new(None),
            unseen_output: false,
            search_error: None,
            input_busy: false,
            output_repaint: OutputRepaint::default(),
            output_search_at: None,
            output_search_pending: false,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            scroll_fraction: 0.0,
            scroll_gesture_active: false,
            scroll_gesture_pinned: false,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            system_notices: Vec::new(),
            log: None,
            image_cache: std::cell::RefCell::default(),
            #[cfg(test)]
            painted_images: std::cell::Cell::new(0),
            x11: None,
            directory: None,
            environment_notes: Vec::new(),
            serial: None,
        }
    }

    /// Feeds bytes as if the shell had produced them.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn feed_for_test(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let was_alternate = self.alternate_screen();
        self.record_output(bytes);
        let effects = self.core.feed(bytes);
        if !bytes.is_empty() && !self.alternate_screen() && self.core.display_offset() > 0 {
            self.unseen_output = true;
        }
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

    /// Opens the right-click menu at `position`, the way a right click on the grid
    /// does, for a capture (R6). Nothing in it is run.
    #[cfg(feature = "ui-capture")]
    pub fn capture_open_menu(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.menu_link = self.hit_test_link(position).map(|link| link.url);
        self.menu = Some(position);
        cx.notify();
    }

    /// Whether the right-click menu is open, for a capture (R6).
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// The visible grid as text, row by row.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn visible_text(&self) -> Vec<String> {
        self.core.snapshot().text()
    }

    /// The last text the pane put on the clipboard.
    #[cfg(test)]
    pub fn last_copy(&self) -> Option<&str> {
        self.last_copy.as_deref()
    }

    /// The kind of selection being kept, even one that covers nothing yet; `None`
    /// while a press has not begun one.
    #[cfg(test)]
    pub fn selection_kind(&self) -> Option<SelectionType> {
        self.core.selection_kind()
    }

    /// Selects from the first character of `from` to the last of `to`, for a capture
    /// (R4, ADR-5).
    ///
    /// Each end is found in the visible grid - the lowest row that holds it - and taken
    /// on its outer half: the left half of the first character, the right half of the
    /// last, the way a drag from the left half of a character takes it. The request is
    /// kept and laid again after a resize, because a new width clears a selection.
    /// Nothing is copied.
    #[cfg(feature = "ui-capture")]
    pub fn capture_select(
        &mut self,
        from: &'static str,
        to: &'static str,
        kind: SelectionType,
        cx: &mut Context<Self>,
    ) {
        self.capture_selection = Some(CaptureSelection { from, to, kind });
        self.apply_capture_selection();
        cx.notify();
    }

    /// Hovers the lowest OSC 8 link on screen, as a pointer resting on it would, so a
    /// capture shows the link's underline and its target (R1). Nothing is opened.
    #[cfg(feature = "ui-capture")]
    pub fn capture_hover_link(&mut self, cx: &mut Context<Self>) -> bool {
        let snapshot = self.core.snapshot();
        let found = snapshot
            .rows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(row, rendered)| {
                rendered.links.first().map(|link| (row, link.columns.start))
            });
        self.hovered_link = found.and_then(|(row, column)| self.core.link_at(row, column));
        self.link_armed = false;
        cx.notify();
        self.hovered_link.is_some()
    }

    /// How many images the terminal keeps, for a capture's readiness check (R4).
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_image_count(&self) -> usize {
        self.core.image_count()
    }

    /// Whether a capture's hovered link is still shown.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_link_hovered(&self) -> bool {
        self.hovered_link.is_some()
    }

    /// Lays the selection a capture asked for on the grid as it is now.
    #[cfg(feature = "ui-capture")]
    fn apply_capture_selection(&mut self) {
        let Some(request) = self.capture_selection else {
            return;
        };
        let snapshot = self.core.snapshot();
        let find = |needle: &str| {
            snapshot
                .rows
                .iter()
                .enumerate()
                .rev()
                .find_map(|(row, cells)| columns_of(&cells.cells, needle).map(|span| (row, span)))
        };
        let (Some((from_row, (from_column, _))), Some((to_row, (_, to_column)))) =
            (find(request.from), find(request.to))
        else {
            self.core.clear_selection();
            return;
        };
        self.core
            .begin_selection(from_row, from_column, Side::Left, request.kind);
        self.core.extend_selection(to_row, to_column, Side::Right);
    }

    /// The selected text, without copying it.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn selected_text(&self) -> Option<String> {
        self.core.selection_text()
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

    /// What this pane remembers about its serial line, when it is one (R21).
    ///
    /// The window reads the state through the strip it draws itself; this is what a
    /// test and the capture probe ask for the same facts.
    #[cfg(test)]
    #[must_use]
    pub fn serial(&self) -> Option<&SerialPane> {
        self.serial.as_ref()
    }

    /// Whether this pane is a serial session.
    #[cfg(any(test, feature = "ui-capture"))]
    #[must_use]
    pub fn is_serial(&self) -> bool {
        self.serial.is_some()
    }

    /// Whether the pane still holds a source it can send to, for a test.
    ///
    /// A serial pane whose session is over must answer `false`: while it answers
    /// `true` the writer thread and the send queue are still alive behind a device
    /// nothing is talking over (R21).
    #[cfg(test)]
    #[must_use]
    pub fn holds_source(&self) -> bool {
        self.source.is_some()
    }

    /// Chooses what the Return key sends, from the next send onwards.
    pub fn set_serial_format(&mut self, format: NewlineFormat, cx: &mut Context<Self>) {
        if let Some(serial) = self.serial.as_mut()
            && serial.format() != format
        {
            serial.set_format(format);
            cx.emit(PaneRequest::SerialFormatChanged(format));
            cx.notify();
        }
    }

    /// Turns the local echo on or off.
    pub fn set_serial_echo(&mut self, echo: bool, cx: &mut Context<Self>) {
        if let Some(serial) = self.serial.as_mut()
            && serial.echo() != echo
        {
            serial.set_echo(echo);
            cx.notify();
        }
    }

    /// Asks the system for the serial port list again.
    ///
    /// It is the refresh control whole job: the answer replaces the one on screen
    /// whether it was read or not, so a device that was just plugged in appears and a
    /// device service that came back starts being readable again.
    pub fn refresh_serial_ports(&mut self, cx: &mut Context<Self>) {
        if let Some(serial) = self.serial.as_mut() {
            serial.refresh_ports();
            cx.notify();
        }
    }

    /// Puts a port list in front of the pane without asking the system.
    ///
    /// The tests use it to drive the situations a machine cannot be made to produce on
    /// demand - a list that could not be read, and an answer with nothing in it - while
    /// the control above stays the only path the application uses.
    #[cfg(test)]
    pub fn show_serial_ports(&mut self, ports: PortScan, cx: &mut Context<Self>) {
        if let Some(serial) = self.serial.as_mut() {
            serial.show_ports(ports);
            cx.notify();
        }
    }

    /// The one line this pane says about its serial line, when it says one.
    #[cfg(test)]
    #[must_use]
    pub fn serial_note(&self) -> Option<&str> {
        self.serial.as_ref().and_then(SerialPane::note)
    }
    /// Increases or decreases the font size by one step.
    pub fn step_font_size(&mut self, larger: bool, cx: &mut Context<Self>) {
        let size = step_font_size(self.font_size, larger);
        self.set_font_size(size, cx);
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        if self.poll_task.is_some() {
            return;
        }
        // A source that says when it has something waits for that; only a serial line,
        // which has no such signal, is polled. An idle shell then wakes nothing.
        let ready = match self.source.as_ref() {
            Some(PaneSource::Remote(session)) => Some(Ready::Ssh(session.output_ready())),
            Some(PaneSource::Chain(session)) => Some(Ready::Ssh(session.output_ready())),
            // GPUI's test executor never fires timers, so the tests have always driven
            // a real shell's output by hand; a wakeup from the PTY's own thread would
            // drain whatever the platform's shell prints in the middle of a test. The
            // wakeup itself is tested where it is made (`pty.rs`).
            #[cfg(not(test))]
            Some(PaneSource::Local(session)) => Some(Ready::Local(session.output_ready())),
            Some(_) => None,
            None => return,
        };
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            let mut search_pending = false;
            // A drain that ran out of time may have left events behind; the next one
            // follows on a timer rather than waiting for a wakeup already spent.
            let mut hurry = false;
            let mut busy = Duration::ZERO;
            loop {
                if hurry && ready.is_some() {
                    // The yield after the busy drain already let input and painting
                    // run; go straight on to what was left behind.
                } else if let Some(ready) = &ready
                    && !search_pending
                {
                    ready.wait().await;
                } else {
                    cx.background_executor().timer(OUTPUT_POLL_INTERVAL).await;
                }
                let started = std::time::Instant::now();
                let result = this.update(cx, |pane, cx| {
                    (pane.drain_events(cx), pane.output_search_pending)
                });
                let Ok((true, pending)) = result else { break };
                search_pending = pending;
                hurry = started.elapsed() >= SSH_DRAIN_BUDGET;
                // A full queue wakes synchronously. A short timer leaves the native
                // event loop idle long enough to dispatch input and paint messages.
                busy += started.elapsed();
                if ready.is_some() && busy >= SSH_DRAIN_BUDGET {
                    // WinRT thread-pool timers can round a 1ms request up to a
                    // clock tick. Rust's Windows sleep uses a waitable timer.
                    #[cfg(windows)]
                    cx.background_executor()
                        .spawn(async { std::thread::sleep(Duration::from_millis(1)) })
                        .await;
                    #[cfg(not(windows))]
                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
                    busy = Duration::ZERO;
                }
            }
        }));
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
        for event in coalesce_output(events) {
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
        let locale = self.locale;
        let Some(source) = self.source.as_mut() else {
            return false;
        };
        let ssh = matches!(source, PaneSource::Remote(_) | PaneSource::Chain(_));
        // A source that wakes its reader drains until it is empty (within the budget):
        // the wakeup that brought this drain may stand for more than one batch.
        let drains_all = ssh || matches!(source, PaneSource::Local(_));
        let deadline = std::time::Instant::now() + SSH_DRAIN_BUDGET;
        let mut events = source.drain(locale);
        // What the line itself reported - a refused send, a frame the driver cut short,
        // bytes it dropped - is taken even when no state event came with it: it is news
        // about the device, and the pane is where a person reads it.
        let line = match self.source.as_mut() {
            Some(PaneSource::Serial(link)) => {
                let mut said = None;
                for note in [
                    link.take_send_problem(),
                    link.take_incomplete(),
                    link.take_loss(),
                ]
                .into_iter()
                .flatten()
                {
                    said = Some(note);
                }
                said
            }
            _ => None,
        };
        if let (Some(note), Some(serial)) = (line, self.serial.as_mut()) {
            serial.say(note);
            cx.notify();
        }
        let was_alternate = self.alternate_screen();
        let mut live = self.state.accepts_input();
        let changed = !events.is_empty();
        // Coalesce plain log traffic. Applications that repaint the screen or
        // change rendition keep their normal display cadence.
        let mut streaming_text = ssh && !was_alternate;
        let mut output_bytes = 0;
        while !events.is_empty() {
            for event in coalesce_output(events) {
                if let SourceEvent::Data(bytes) = event {
                    if streaming_text && bytes.contains(&0x1b) {
                        streaming_text = false;
                    }
                    output_bytes += bytes.len();
                    self.wake_cursor();
                    self.record_output(&bytes);
                    let effects = self.core.feed(&bytes);
                    if !bytes.is_empty()
                        && !self.alternate_screen()
                        && self.core.display_offset() > 0
                    {
                        self.unseen_output = true;
                    }
                    self.apply_effects(effects, cx);
                    self.output_search_pending = self.search_open;
                    live = self.state.accepts_input();
                } else {
                    streaming_text = false;
                    live = self.handle_event(event, cx);
                }
            }
            if !drains_all || !live || std::time::Instant::now() >= deadline {
                break;
            }
            let Some(source) = self.source.as_mut() else {
                break;
            };
            events = source.drain(locale);
        }
        // A continuous log stream must not rescan all scrollback every 12 ms.
        // User edits/navigation still refresh immediately. Flush the pending scan
        // on a quiet poll or exit too, so the final output is never left stale.
        let search_due = self
            .output_search_at
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(100));
        if self.output_search_pending
            && (search_due || !live || self.alternate_screen() != was_alternate)
        {
            self.recount_after_output(was_alternate);
            self.output_search_pending = false;
            self.output_search_at = Some(std::time::Instant::now());
            cx.notify();
        }
        if changed {
            if streaming_text && !self.alternate_screen() {
                self.repaint_ssh_output(output_bytes, cx);
            } else {
                cx.notify();
            }
        }
        live
    }

    /// Echo and other small replies repaint immediately. During a flood, draw at
    /// most once per interval and flush the final update even if no more data arrives.
    fn repaint_ssh_output(&mut self, bytes: usize, cx: &mut Context<Self>) {
        const FRAME_INTERVAL: Duration = Duration::from_millis(32);
        const BURST_BYTES: usize = 4096;
        if self.output_repaint.pending {
            return;
        }
        let now = cx.background_executor().now();
        let remaining = self
            .output_repaint
            .deadline
            .map_or(Duration::ZERO, |deadline| {
                deadline.saturating_duration_since(now)
            });
        if remaining.is_zero() {
            self.output_repaint.deadline = Some(now + FRAME_INTERVAL);
            self.output_repaint.bytes = bytes;
            cx.notify();
            return;
        }
        self.output_repaint.bytes = self.output_repaint.bytes.saturating_add(bytes);
        if self.output_repaint.bytes < BURST_BYTES {
            cx.notify();
            return;
        }
        self.output_repaint.pending = true;
        let timer = cx.background_executor().timer(remaining);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |pane, cx| {
                pane.output_repaint.pending = false;
                pane.output_repaint.deadline =
                    Some(cx.background_executor().now() + FRAME_INTERVAL);
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_event(&mut self, event: SourceEvent, cx: &mut Context<Self>) -> bool {
        match event {
            SourceEvent::Data(bytes) => {
                let was_alternate = self.alternate_screen();
                self.record_output(&bytes);
                let effects = self.core.feed(&bytes);
                if !bytes.is_empty() && !self.alternate_screen() && self.core.display_offset() > 0 {
                    self.unseen_output = true;
                }
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
                // not be started" would send the user to fix a launch that worked, so
                // the pane states the loss as a loss (R17).
                self.state = PaneState::Lost(self.origin.lost_failure(reason));
                false
            }
            SourceEvent::X11(status) => {
                // A display that failed does not end the session: the shell keeps
                // working and the status is news for the session bar (R11).
                self.x11 = Some(status);
                self.state.accepts_input()
            }
            SourceEvent::Platform(platform) => {
                cx.emit(PaneRequest::Platform(platform));
                self.state.accepts_input()
            }
        }
    }

    fn apply_effects(&mut self, effects: TerminalEffects, cx: &mut Context<Self>) {
        // A failed log write is noticed with the next output, which is when it
        // happened; the terminal itself carries on (R3).
        self.check_log(cx);
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
        // A program's notification request and a long command's end are kept for the
        // window, which alone knows whether this terminal is in front (R2). Bounded, so
        // a window that is not collecting cannot make a pane grow without limit.
        for notice in effects.notices {
            self.push_system_notice(PaneNotice::Program(notice));
        }
        if let Some(finished) = effects.finished_command {
            self.push_system_notice(PaneNotice::Finished(finished));
        }
        if let Some(directory) = effects.directory {
            // The shell stated where it is. The report is kept until the window reads
            // it, so one that arrived between two frames is not lost; keeping only the
            // newest is right, because each report replaces the previous position of
            // the same shell (R15).
            self.directory = Some(directory);
        }
    }

    /// Takes the background notices this terminal produced since the last call.
    ///
    /// The window collects them, so the same bell is never marked twice. Reading them
    /// does not touch the grid, the blocks or the session state.
    pub fn take_attention(&mut self) -> TerminalAttention {
        std::mem::take(&mut self.attention)
    }

    /// Hands output to the running log, if there is one (R3). Called with every chunk
    /// the grid receives, before it is parsed, so the log is the same stream.
    fn record_output(&self, bytes: &[u8]) {
        if let Some(log) = &self.log {
            log.write(bytes);
        }
    }

    /// Stops a log whose writing failed and tells the window why (R3). The terminal
    /// is not touched: the session goes on, only the file stops growing.
    fn check_log(&mut self, cx: &mut Context<Self>) {
        let Some(reason) = self.log.as_mut().and_then(SessionLog::poll_error) else {
            return;
        };
        if let Some(log) = self.log.take() {
            let path = log.path().to_owned();
            drop(log.stop());
            cx.emit(PaneRequest::LogFailed { path, reason });
            cx.notify();
        }
    }

    /// Starts writing this terminal's output to `path` (R3), replacing a log that
    /// was running.
    ///
    /// # Errors
    ///
    /// Returns why the file could not be created; nothing is started then.
    pub fn start_log(
        &mut self,
        path: &std::path::Path,
        format: LogFormat,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        drop(self.log.take());
        let log = SessionLog::start(path, format).map_err(|error| error.to_string())?;
        self.log = Some(log);
        cx.notify();
        Ok(())
    }

    /// Stops the running log, writing what it was given (R3).
    ///
    /// A terminal without a log is left untouched: the window asks every terminal of
    /// an ending session, and a redraw requested here would make it ask again.
    pub fn stop_log(&mut self, cx: &mut Context<Self>) -> Option<LogReport> {
        let report = self.log.take().map(SessionLog::stop)?;
        cx.notify();
        Some(report)
    }

    /// The file this terminal is logging to and how, while it is (R3).
    #[must_use]
    pub fn logging(&self) -> Option<(&std::path::Path, LogFormat)> {
        self.log.as_ref().map(|log| (log.path(), log.format()))
    }

    /// The name a new log of this terminal is offered under (R3):
    /// `<source>-YYYYMMDD-HHMMSSZ.log`, the time in UTC and the source label
    /// reduced to characters every file system takes.
    #[must_use]
    pub fn default_log_name(&self, now_unix: u64) -> String {
        default_log_name(self.origin.label(), now_unix)
    }

    /// Keeps one notification for the window, dropping the oldest past a small bound.
    fn push_system_notice(&mut self, notice: PaneNotice) {
        const MOST: usize = 16;
        if self.system_notices.len() >= MOST {
            self.system_notices.remove(0);
        }
        self.system_notices.push(notice);
    }

    /// Takes the desktop notifications this terminal produced since the last call
    /// (R2). The window decides about each one.
    pub fn take_system_notices(&mut self) -> Vec<PaneNotice> {
        std::mem::take(&mut self.system_notices)
    }

    /// Takes what X11 forwarding reported since the last call (R11).
    ///
    /// The window keeps it beside the session, because the session bar is where a
    /// remote program's access to this machine's display is stated. Reading it does
    /// not touch the grid or the session state.
    pub fn take_x11_status(&mut self) -> Option<X11Status> {
        self.x11.take()
    }

    /// Takes the working directory the shell last reported with OSC 7 (R15).
    ///
    /// Reading it does not touch the grid, the blocks or the session state, and a
    /// shell that never reported one leaves this `None` rather than the directory a
    /// login usually starts in.
    pub fn take_directory_report(&mut self) -> Option<String> {
        self.directory.take()
    }

    /// The notices waiting to be collected, for the tests that check a tab mark.
    #[cfg(test)]
    #[must_use]
    pub fn pending_attention_for_test(&self) -> TerminalAttention {
        self.attention
    }

    /// Sends bytes the user produced: typed text, a key, a paste, a saved command.
    ///
    /// Two rules of a serial session live here, and both are about the user half of the
    /// line only (R21):
    ///
    /// * the chosen line ending is written for every break, so Return sends what the
    ///   device expects;
    /// * the local echo shows what was sent in this pane, because a device that does not
    ///   echo would otherwise leave the user typing blind.
    ///
    /// What the device returns is never passed through either rule: it takes `write`
    /// straight into the grid, so the transcript cannot gain a break the wire never
    /// carried. The echo is drawn from this path alone, which is what keeps an echoed
    /// byte from being read as a byte the device sent back - the pane only shows both
    /// when both really happened, which is the case a device that also echoes produces.
    fn write_user(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let Some(serial) = self.serial.as_ref() else {
            if self.write(bytes, cx) {
                self.core.note_command_input(bytes);
            }
            return;
        };
        let format = serial.format();
        let echo = serial.echo();
        let outgoing = format.apply(bytes);
        if !self.state.accepts_input() || outgoing.is_empty() {
            return;
        }
        let accepted = self.write(&outgoing, cx);
        if accepted && echo {
            self.echo(&format.echo(&outgoing), cx);
        }
    }

    /// Shows a copy of what was just sent, as the local echo.
    ///
    /// The bytes go into the same grid the device writes into, because that is what a
    /// user reads; what keeps them from being mistaken for device output is that this is
    /// the only path that draws them, and it is called from the send path alone.
    fn echo(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        let was_alternate = self.alternate_screen();
        // The local echo is part of what the terminal shows, so it is part of the
        // log too; keys that were not echoed never are (R3).
        self.record_output(bytes);
        let effects = self.core.feed(bytes);
        self.apply_effects(effects, cx);
        self.recount_after_output(was_alternate);
        cx.notify();
    }

    fn write(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        if bytes.is_empty() || !self.state.accepts_input() {
            return false;
        }
        // Only a test keeps a copy of the byte stream; a release build would grow
        // it for the life of the session.
        #[cfg(test)]
        self.sent.extend_from_slice(bytes);
        let locale = self.locale;
        let Some(source) = self.source.as_mut() else {
            return true;
        };
        match source.write(bytes, locale) {
            Ok(accepted) => {
                if !matches!(source, PaneSource::Serial(_)) && self.input_busy == accepted {
                    self.input_busy = !accepted;
                    cx.notify();
                }
                accepted
            }
            Err(error) => {
                // A write that fails happened on a session that was live: the program ran
                // and the channel is gone, which is a loss and not a launch failure.
                let failure = self.origin.lost_failure(error);
                self.state = PaneState::Lost(failure);
                cx.notify();
                false
            }
        }
    }

    fn reveal_input(&mut self, cx: &mut Context<Self>) {
        self.core.scroll_to_bottom();
        self.scroll_fraction = 0.0;
        self.scroll_gesture_pinned = self.scroll_gesture_active;
        self.unseen_output = false;
        cx.notify();
    }

    /// Clears both the visible primary screen and its scrollback history.
    pub fn clear_terminal_and_scrollback(&mut self, cx: &mut Context<Self>) {
        if self.alternate_screen() {
            return;
        }
        self.core.clear_screen_and_scrollback();
        self.scroll_fraction = 0.0;
        self.scroll_gesture_pinned = false;
        self.unseen_output = false;
        self.hovered_link = None;
        self.menu_link = None;
        if self.search_open {
            self.search_scroll = None;
            self.refresh_search(false);
        }
        cx.notify();
    }

    /// User input reveals the live cursor. Protocol replies and mouse reports
    /// use `write` directly so they cannot disturb someone reading history.
    fn write_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() || !self.state.accepts_input() {
            return;
        }
        self.wake_cursor();
        self.reveal_input(cx);
        self.write_user(bytes, cx);
    }

    /// Sends what a key press means, or scrolls for the view-only combinations.
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if (self.menu.is_some() || self.more_menu.is_some()) && event.keystroke.key == "escape" {
            self.menu = None;
            self.menu_link = None;
            self.more_menu = None;
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
        // A terminal whose session is over takes no keys of its own, so Tab moves on to
        // the banner above it and its commands are reached without the pointer (ADR-6).
        if keystroke.key == "tab"
            && plain
            && !modifiers.shift
            && !self.state.accepts_input()
            && let Some(focus) = self.banner_focus.borrow().clone()
        {
            window.focus(&focus);
            cx.stop_propagation();
            return;
        }

        // While the find bar holds the keyboard, every editing, navigation and control
        // key belongs to the query rather than the shell (R3): Escape closes the bar and
        // hands the terminal back, Enter/Shift+Enter step through hits, Alt+C/Alt+R toggle
        // switches, arrow/Home/End/Backspace/Delete/Ctrl editing keys edit the query in
        // place, and no unhandled control or navigation key may fall through to
        // `encode_key -> write_input` or pull a parked viewport back to the bottom.
        if self.search_captures_keyboard() {
            let bar_alt = modifiers.alt && !modifiers.control && !modifiers.platform;
            let ctrl_only = modifiers.control && !modifiers.alt && !modifiers.platform;
            let secondary_only = ((modifiers.control && !modifiers.platform)
                || (modifiers.platform && !modifiers.control))
                && !modifiers.alt;
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
                    self.delete_search_backward(cx);
                    cx.stop_propagation();
                    return;
                }
                "backspace" | "w" if ctrl_only && !modifiers.shift => {
                    self.delete_search_word_backward(cx);
                    cx.stop_propagation();
                    return;
                }
                "delete" if plain => {
                    self.delete_search_forward(cx);
                    cx.stop_propagation();
                    return;
                }
                "left" if plain => {
                    if let Some(sel) = self.take_search_selection() {
                        self.search_cursor = sel.start;
                    } else {
                        self.clamp_search_cursor();
                        self.search_cursor = self.prev_search_boundary(self.search_cursor);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "left" if ctrl_only => {
                    let start = self.take_search_selection().map_or_else(
                        || {
                            self.clamp_search_cursor();
                            self.search_cursor
                        },
                        |sel| sel.start,
                    );
                    self.search_cursor = self.prev_search_word_boundary(start);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "right" if plain => {
                    if let Some(sel) = self.take_search_selection() {
                        self.search_cursor = sel.end;
                    } else {
                        self.clamp_search_cursor();
                        self.search_cursor = self.next_search_boundary(self.search_cursor);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "right" if ctrl_only => {
                    let start = self.take_search_selection().map_or_else(
                        || {
                            self.clamp_search_cursor();
                            self.search_cursor
                        },
                        |sel| sel.end,
                    );
                    self.search_cursor = self.next_search_word_boundary(start);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "home" if plain => {
                    self.search_selection = None;
                    self.search_cursor = 0;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "end" if plain => {
                    self.search_selection = None;
                    self.search_cursor = self.search_query.len();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "a" if secondary_only && !modifiers.shift => {
                    if self.search_query.is_empty() {
                        self.search_selection = None;
                        self.search_cursor = 0;
                    } else {
                        let len = self.search_query.len();
                        self.search_selection = Some(0..len);
                        self.search_cursor = len;
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "c" if secondary_only && !modifiers.shift => {
                    if let Some(sel) = self.normalized_search_selection() {
                        let text = self.search_query[sel].to_owned();
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        self.last_copy = Some(text);
                        cx.notify();
                    }
                    cx.stop_propagation();
                    return;
                }
                "e" if ctrl_only && !modifiers.shift => {
                    self.search_selection = None;
                    self.search_cursor = self.search_query.len();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "u" if ctrl_only && !modifiers.shift => {
                    let cut_to = self.take_search_selection().map_or_else(
                        || {
                            self.clamp_search_cursor();
                            self.search_cursor
                        },
                        |sel| sel.end,
                    );
                    if cut_to > 0 {
                        self.search_query.drain(..cut_to);
                        self.search_cursor = 0;
                        self.refresh_search(true);
                        cx.notify();
                    }
                    cx.stop_propagation();
                    return;
                }
                "k" if ctrl_only && !modifiers.shift => {
                    let cut_from = self.take_search_selection().map_or_else(
                        || {
                            self.clamp_search_cursor();
                            self.search_cursor
                        },
                        |sel| sel.start,
                    );
                    if cut_from < self.search_query.len() {
                        self.search_query.truncate(cut_from);
                        self.search_cursor = cut_from;
                        self.refresh_search(true);
                        cx.notify();
                    }
                    cx.stop_propagation();
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
                "g" if ctrl_only && modifiers.shift => {
                    self.next_search_hit(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {
                    if modifiers.platform {
                        return;
                    }
                    // Consume any control, alt, navigation or function key so it can
                    // never fall through to the shell. Plain printable characters stay
                    // un-stopped so GPUI's input handler commits them into the query.
                    let encoded = Key::from_name(&keystroke.key)
                        .is_some_and(|key| encode_key(key, modifiers, self.core.mode()).is_some());
                    if modifiers.control || modifiers.alt || encoded {
                        cx.stop_propagation();
                    }
                    return;
                }
            }
        }

        // The terminal's own clipboard and search keys arrive as actions, so the
        // settings page can rebind or unbind them (R5). These confirm or cancel a
        // paste and step through search hits; they are not Ctrl+L, which stays
        // program input below. Ctrl+C reaches here only when nothing is selected,
        // and then it is the program's interrupt.
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
                    if self.core.display_offset() == 0 {
                        self.unseen_output = false;
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }

        if plain
            && !modifiers.shift
            && matches!(keystroke.key.as_str(), "right" | "escape")
            && let Some(suffix) = self.history_completion()
        {
            if keystroke.key == "right" {
                let text = suffix.to_owned();
                self.write_input(text.as_bytes(), cx);
            } else {
                self.core.dismiss_history_completion();
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(key) = Key::from_name(&keystroke.key)
            && let Some(bytes) = encode_key(key, modifiers, self.core.mode())
        {
            self.write_input(&bytes, cx);
            // The key was consumed as a terminal key, so the text-input path must not
            // run for it as well: Return would otherwise be sent twice.
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Sends text to the program as if it had been typed.
    pub fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.write_input(text.as_bytes(), cx);
    }

    /// Turns the colouring of addresses, links and failures in plain output on or off.
    pub fn set_highlight(&mut self, on: bool, cx: &mut Context<Self>) {
        self.core.set_highlight(on);
        cx.notify();
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

    /// The keyboard copy: copies a selection and clears it, so a second press is the
    /// program's interrupt. Returns whether there was anything to copy.
    fn copy_selection_from_key(&mut self, cx: &mut Context<Self>) -> bool {
        if self.search_captures_keyboard() {
            if let Some(sel) = self.normalized_search_selection() {
                let text = self.search_query[sel].to_owned();
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                self.last_copy = Some(text);
                cx.notify();
            }
            return true;
        }
        if !self
            .core
            .selection_text()
            .is_some_and(|text| !text.is_empty())
        {
            // A highlight over blank cells holds nothing to copy; it goes, and the
            // key is the program's.
            if self.core.has_selection() {
                self.core.clear_selection();
                cx.notify();
            }
            return false;
        }
        self.copy_selection(cx);
        self.core.clear_selection();
        cx.notify();
        true
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
        if self.search_captures_keyboard() {
            if let Some(text) = item.text().filter(|text| !text.is_empty()) {
                // Line breaks have no meaning in a query: a pasted block becomes one
                // line, exactly as a single-line field would take it.
                let query = text.lines().collect::<Vec<_>>().join(" ");
                self.append_search_query(&query, cx);
            }
            // Image-only or empty clipboards are ignored while the find bar owns the
            // keyboard so neither Ctrl+V (0x16) nor a remote image upload leaks to the
            // shell behind the bar (R3).
            return;
        }
        if let Some(text) = item.text().filter(|text| !text.is_empty()) {
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
            PaneOrigin::Local(_) => self.write_input(&[0x16], cx),
            PaneOrigin::Remote { .. } => cx.emit(PaneRequest::PasteImage(image)),
            // A serial device has no clipboard to read and no channel to upload over, so
            // an image pasted here goes nowhere and is not claimed to have gone
            // anywhere (R21: no file capability for a serial session).
            PaneOrigin::Serial { .. } => {}
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
            // A serial port transfers bytes, not files: a dropped file has nowhere to
            // be uploaded to and no shell to receive its path, so nothing is written.
            PaneOrigin::Serial { .. } => {}
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
        if text.is_empty() || !self.state.accepts_input() {
            return;
        }
        if paste_needs_confirmation(text) {
            self.set_pending_paste(Some(PendingPaste {
                text: text.to_owned(),
                target: self.origin.label().to_owned(),
                execute: false,
            }));
            cx.notify();
            return;
        }
        self.set_pending_paste(None);
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
            self.set_pending_paste(Some(PendingPaste {
                text: trimmed.to_owned(),
                target: self.origin.label().to_owned(),
                execute,
            }));
            cx.notify();
            return SnippetOffer::Waiting;
        }
        self.set_pending_paste(None);
        self.write_snippet(trimmed, execute, cx);
        cx.notify();
        SnippetOffer::Written
    }

    /// Writes the held paste into this pane only.
    ///
    /// Returns whether a paste was waiting. The bytes go through this pane's
    /// source, so another session cannot receive them.
    pub fn confirm_paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pending) = self.set_pending_paste(None) else {
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

    /// Writes `text` in place of the held paste: the user changed it in the dialog.
    ///
    /// What the dialog showed is what is written; an empty text writes nothing and
    /// drops the paste. Returns whether a paste was waiting.
    pub fn confirm_edited_paste(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(mut pending) = self.set_pending_paste(None) else {
            return false;
        };
        if !text.is_empty() {
            pending.text = text.to_owned();
            if pending.execute {
                self.write_snippet(&pending.text, true, cx);
            } else {
                self.write_paste(&pending.text, cx);
            }
        }
        cx.notify();
        true
    }

    /// Drops a held paste. Nothing is written.
    pub fn cancel_paste(&mut self, cx: &mut Context<Self>) {
        if self.set_pending_paste(None).is_some() {
            cx.notify();
        }
    }

    fn set_pending_paste(&mut self, pending: Option<PendingPaste>) -> Option<PendingPaste> {
        self.paste_waiting
            .store(pending.is_some(), std::sync::atomic::Ordering::Relaxed);
        std::mem::replace(&mut self.pending_paste, pending)
    }

    /// Shared lock-free flag reflecting [`Self::awaits_paste_confirmation`] without borrowing the pane entity.
    #[must_use]
    pub fn paste_waiting_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.paste_waiting.clone()
    }

    /// The session a held paste would be sent to, when one is waiting.
    ///
    /// The window draws that answer in its dialog, so it is not test-only: the same
    /// value the pane holds is the value the dialog names and confirms into.
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
    #[must_use]
    pub fn pending_paste_text(&self) -> Option<&str> {
        self.pending_paste.as_ref().map(|paste| paste.text.as_str())
    }

    /// Whether the held paste is a saved command that will run once it is confirmed.
    #[must_use]
    pub fn pending_paste_runs(&self) -> bool {
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
        self.write_input(&bytes, cx);
        debug_assert_eq!(self.core.command_blocks(), before);
        cx.notify();
    }

    fn write_paste(&mut self, text: &str, cx: &mut Context<Self>) {
        let bytes = encode_paste(text, self.core.mode());
        self.write_input(&bytes, cx);
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
        self.write_input(&bytes, cx);
    }

    fn alternate_screen(&self) -> bool {
        self.core.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the search bar can open: a full-screen program owns its screen, so the
    /// palette offers no search there rather than an entry that does nothing.
    pub(crate) fn can_search(&self) -> bool {
        !self.alternate_screen()
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

    /// Opens, re-focuses or closes the find bar (R1).
    ///
    /// A full-screen program owns the screen and its own keys, so an alternate-screen
    /// terminal never opens one: its contents are not this pane's history, and the
    /// keys the bar would take have to reach the program. When the bar is already open
    /// on screen while the terminal holds the keyboard, the shortcut returns focus to
    /// the query without discarding it; pressing it while the query already holds focus
    /// closes the bar.
    pub(crate) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.alternate_screen() {
            return;
        }
        if self.search_open {
            if self.search_focused {
                self.close_search(window, cx);
            } else {
                self.search_focused = true;
                self.clamp_search_cursor();
                window.focus(&self.focus);
                cx.notify();
            }
        } else {
            self.open_search(window, cx);
        }
    }

    fn search_captures_keyboard(&self) -> bool {
        self.search_focused && self.search_open && !self.alternate_screen()
    }

    fn clamp_char_boundary(&self, index: usize) -> usize {
        let mut clamped = index.min(self.search_query.len());
        while clamped > 0 && !self.search_query.is_char_boundary(clamped) {
            clamped -= 1;
        }
        clamped
    }

    fn clamp_search_cursor(&mut self) {
        self.search_cursor = self.clamp_char_boundary(self.search_cursor);
    }

    fn normalized_search_selection(&self) -> Option<Range<usize>> {
        let sel = self.search_selection.as_ref()?;
        let start = self.clamp_char_boundary(sel.start.min(sel.end));
        let end = self.clamp_char_boundary(sel.start.max(sel.end));
        (start < end).then_some(start..end)
    }

    fn take_search_selection(&mut self) -> Option<Range<usize>> {
        let normalized = self.normalized_search_selection();
        self.search_selection = None;
        normalized
    }

    fn prev_search_boundary(&self, cursor: usize) -> usize {
        self.search_query[..cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_search_boundary(&self, cursor: usize) -> usize {
        self.search_query[cursor..]
            .chars()
            .next()
            .map_or(cursor, |ch| cursor + ch.len_utf8())
    }

    fn prev_search_word_boundary(&self, cursor: usize) -> usize {
        let mut pos = cursor;
        while pos > 0 {
            let prev = self.prev_search_boundary(pos);
            let ch = self.search_query[prev..pos].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            pos = prev;
        }
        while pos > 0 {
            let prev = self.prev_search_boundary(pos);
            let ch = self.search_query[prev..pos].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            pos = prev;
        }
        pos
    }

    fn next_search_word_boundary(&self, cursor: usize) -> usize {
        let len = self.search_query.len();
        let mut pos = cursor.min(len);
        while pos < len {
            let next = self.next_search_boundary(pos);
            let ch = self.search_query[pos..next].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            pos = next;
        }
        while pos < len {
            let next = self.next_search_boundary(pos);
            let ch = self.search_query[pos..next].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            pos = next;
        }
        pos
    }

    fn delete_search_backward(&mut self, cx: &mut Context<Self>) {
        if let Some(sel) = self.take_search_selection() {
            self.search_query.drain(sel.clone());
            self.search_cursor = sel.start;
            self.refresh_search(true);
            cx.notify();
            return;
        }
        self.clamp_search_cursor();
        if self.search_cursor == 0 {
            return;
        }
        let prev = self.prev_search_boundary(self.search_cursor);
        self.search_query.drain(prev..self.search_cursor);
        self.search_cursor = prev;
        self.refresh_search(true);
        cx.notify();
    }

    fn delete_search_word_backward(&mut self, cx: &mut Context<Self>) {
        if let Some(sel) = self.take_search_selection() {
            self.search_query.drain(sel.clone());
            self.search_cursor = sel.start;
            self.refresh_search(true);
            cx.notify();
            return;
        }
        self.clamp_search_cursor();
        if self.search_cursor == 0 {
            return;
        }
        let prev = self.prev_search_word_boundary(self.search_cursor);
        self.search_query.drain(prev..self.search_cursor);
        self.search_cursor = prev;
        self.refresh_search(true);
        cx.notify();
    }

    fn delete_search_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(sel) = self.take_search_selection() {
            self.search_query.drain(sel.clone());
            self.search_cursor = sel.start;
            self.refresh_search(true);
            cx.notify();
            return;
        }
        self.clamp_search_cursor();
        if self.search_cursor >= self.search_query.len() {
            return;
        }
        let next = self.next_search_boundary(self.search_cursor);
        self.search_query.drain(self.search_cursor..next);
        self.refresh_search(true);
        cx.notify();
    }

    fn clear_search_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_query.clear();
        self.search_cursor = 0;
        self.search_selection = None;
        self.marked = None;
        self.search_focused = true;
        self.refresh_search(false);
        window.focus(&self.focus);
        cx.notify();
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search_focused = true;
        self.search_selection = None;
        self.search_cursor = self.search_query.len();
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
        self.search_cursor = 0;
        self.search_selection = None;
        self.marked = None;
        self.search_hits.clear();
        self.search_at = 0;
        self.search_mark = None;
        self.search_error = None;
        match self.search_scroll.take() {
            Some(anchor) => self.scroll_to_offset(self.core.offset_of(anchor)),
            None => self.core.scroll_to_bottom(),
        }
        if self.core.display_offset() == 0 {
            self.unseen_output = false;
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
    /// Opens the find bar with a query, for a capture (R2).
    ///
    /// The bar is the one the keyboard opens; only its query is supplied, so the shot
    /// shows the real bar over the real grid and nothing is searched on a server.
    #[cfg(feature = "ui-capture")]
    pub fn capture_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_open {
            self.open_search(window, cx);
        }
        self.append_search_query(text, cx);
    }

    /// Whether the find bar is open with a query, for a capture (R2).
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_search_open(&self) -> bool {
        self.search_open && !self.search_query.is_empty()
    }

    fn append_search_query(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if let Some(sel) = self.take_search_selection() {
            self.search_query.drain(sel.clone());
            self.search_cursor = sel.start;
        } else {
            self.clamp_search_cursor();
        }
        self.search_query.insert_str(self.search_cursor, text);
        self.search_cursor += text.len();
        self.refresh_search(true);
        cx.notify();
    }

    /// Updates whether the enclosing window is in distraction-free focus mode.
    pub fn set_focus_mode(&self, on: bool) {
        self.focus_mode.set(on);
        if on {
            *self.session_slot.borrow_mut() = None;
            self.bar_has_slot.set(false);
        }
    }

    /// Toggles focus mode directly on this pane for isolated unit tests.
    #[cfg(test)]
    pub fn set_focus_mode_for_test(&mut self, on: bool, cx: &mut Context<Self>) {
        self.set_focus_mode(on);
        if on {
            self.more_menu = None;
        }
        cx.notify();
    }

    /// Whether new primary-screen output arrived while parked in scrollback.
    #[cfg(test)]
    #[must_use]
    pub fn unseen_output_for_test(&self) -> bool {
        self.unseen_output
    }

    /// External URLs opened by this pane during a test.
    #[cfg(test)]
    #[must_use]
    pub fn opened_urls_for_test(&self) -> &[String] {
        &self.opened_urls
    }

    /// Currently underlined `Ctrl/⌘`-hovered link URL, if any.
    #[cfg(test)]
    #[must_use]
    pub fn hovered_link_for_test(&self) -> Option<&str> {
        self.hovered_link.as_ref().map(|link| link.url.as_str())
    }

    /// Byte offset of the caret inside the search query, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn search_cursor_for_test(&self) -> usize {
        self.search_cursor
    }

    /// Selected byte range inside the search query, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn search_selection_for_test(&self) -> Option<Range<usize>> {
        self.normalized_search_selection()
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
        // Images are measured in these units, so their rows match the text (R4).
        self.core.set_cell_pixels(cell.width, cell.height);
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
        // A new width clears the selection; a capture's is laid on the new grid again.
        #[cfg(feature = "ui-capture")]
        self.apply_capture_selection();
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

    /// The viewport cell under a window position, as `(row, column)`, and the half
    /// of that cell the position is in.
    ///
    /// A position outside the grid is clamped to it: left of the grid is the first
    /// column's left half, right of it the last column's right half, and above or
    /// below it the first or last row. A drag that leaves the grid, or the window,
    /// keeps selecting to the edge it left by. A position exactly on the middle of a
    /// cell is its left half, as in other terminals.
    fn cell_at(&self, position: gpui::Point<Pixels>) -> Option<((usize, usize), Side)> {
        let bounds = self.viewport?;
        let cell = self.cell.sanitized();
        let cols = self.grid.cols.max(1);
        let rows = self.grid.rows.max(1);
        let x = f32::from(position.x) - f32::from(bounds.origin.x);
        let y = f32::from(position.y) - f32::from(bounds.origin.y);
        let row = if y <= 0.0 {
            0
        } else {
            ((y / cell.height) as usize).min(rows - 1)
        };
        if x <= 0.0 {
            return Some(((row, 0), Side::Left));
        }
        let column = (x / cell.width) as usize;
        if column >= cols {
            return Some(((row, cols - 1), Side::Right));
        }
        let within = x - column as f32 * cell.width;
        let side = if within > cell.width / 2.0 {
            Side::Right
        } else {
            Side::Left
        };
        Some(((row, column), side))
    }

    /// The viewport cell under a window position, for mouse reports.
    fn report_cell_at(&self, position: gpui::Point<Pixels>) -> Option<(usize, usize)> {
        self.cell_at(position).map(|(cell, _side)| cell)
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
        let Some((row, column)) = self.report_cell_at(position) else {
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

    /// Strict cell hit-test inside the measured viewport without clamping outside bounds.
    fn strict_cell_at(&self, position: gpui::Point<Pixels>) -> Option<(usize, usize)> {
        let bounds = self.viewport?;
        if !bounds.contains(&position) {
            return None;
        }
        let cell = self.cell.sanitized();
        let cols = self.grid.cols.max(1);
        let rows = self.grid.rows.max(1);
        let x = f32::from(position.x - bounds.origin.x);
        let y = f32::from(position.y - bounds.origin.y);
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let row = (y / cell.height) as usize;
        let column = (x / cell.width) as usize;
        if row >= rows || column >= cols {
            return None;
        }
        Some((row, column))
    }

    /// The link under `position`: an OSC 8 hyperlink on either screen, else an
    /// openable `http(s)://` address in primary-screen text (R1).
    fn hit_test_link(&self, position: gpui::Point<Pixels>) -> Option<DetectedLink> {
        let (row, column) = self.strict_cell_at(position)?;
        self.core
            .link_at(row, column)
            .filter(|link| link.kind == LinkKind::Osc8 || is_openable_http_url(&link.url))
    }

    fn link_modifier_active(modifiers: &gpui::Modifiers) -> bool {
        modifiers.number_of_modifiers() == 1 && modifiers.secondary()
    }

    /// Opens a link target, or refuses it and says so (R1).
    ///
    /// Recognized text is always an `http(s)://` address. An OSC 8 target is the
    /// program's choice, so only web, mail and this computer's files are opened; any
    /// other is named in a message and nothing is launched.
    fn open_link(&mut self, url: &str, cx: &mut Context<Self>) -> bool {
        if let Err(refusal) = osc8_target_allowed(url) {
            cx.emit(PaneRequest::LinkRefused(refusal));
            return false;
        }
        #[cfg(test)]
        self.opened_urls.push(url.to_owned());
        cx.open_url(url);
        cx.notify();
        true
    }

    /// Looks the hovered link up again at the last pointer position and modifiers;
    /// returns whether what is hovered changed.
    ///
    /// The hover is in viewport cells, and the text under a still pointer moves when
    /// the history scrolls, output arrives or the grid is resized. Each of those
    /// redraws the pane, and the redraw asks again here, so an underline or a target
    /// is never left on cells that now hold other text (R1).
    fn refresh_hovered_link(&mut self) -> bool {
        let modifiers = self.last_hover_modifiers;
        // A program's OSC 8 link is shown on plain hover, so its target can be read
        // before anything is opened; recognized text only lights up with the
        // modifier that opens it, as before (R1).
        let next = if self.drag.is_none() {
            self.last_hover_position
                .and_then(|pos| self.hit_test_link(pos))
                .filter(|link| {
                    link.kind == LinkKind::Osc8 || Self::link_modifier_active(&modifiers)
                })
        } else {
            None
        };
        let armed = next.is_some() && Self::link_modifier_active(&modifiers);
        if self.hovered_link == next && self.link_armed == armed {
            return false;
        }
        self.hovered_link = next;
        self.link_armed = armed;
        true
    }

    fn update_hovered_link(
        &mut self,
        position: Option<gpui::Point<Pixels>>,
        modifiers: &gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        self.last_hover_position = position;
        self.last_hover_modifiers = *modifiers;
        if self.refresh_hovered_link() {
            cx.notify();
        }
    }

    fn on_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        self.update_hovered_link(self.last_hover_position, &event.modifiers, cx);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.search_focused = false;
        self.search_selection = None;
        self.menu = None;
        self.menu_link = None;
        if event.click_count == 1
            && Self::link_modifier_active(&event.modifiers)
            && let Some(link) = self.hit_test_link(event.position)
        {
            self.open_link(&link.url, cx);
            return;
        }
        if self.report_mouse(
            TerminalMouseButton::Left,
            MouseAction::Press,
            event.position,
            &event.modifiers,
            cx,
        ) {
            self.mouse_held = Some(TerminalMouseButton::Left);
            self.last_report_cell = self.report_cell_at(event.position);
            return;
        }
        let Some(((row, column), side)) = self.cell_at(event.position) else {
            return;
        };
        // A double click selects a word, a triple click the line; Alt (Option on
        // macOS) drags a block, the same columns from every row.
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            count if count >= 3 => SelectionType::Lines,
            _ if event.modifiers.alt => SelectionType::Block,
            _ => SelectionType::Simple,
        };
        let mut drag = SelectionDrag {
            origin: event.position,
            anchor: ((row, column), side),
            anchor_line: Some(self.core.anchor_viewport_row(row)),
            kind,
            started: false,
            pointer: event.position,
        };
        if matches!(kind, SelectionType::Semantic | SelectionType::Lines) {
            // A word or a line is chosen by the click itself.
            self.core.begin_selection(row, column, side, kind);
            drag.started = true;
        } else if event.modifiers.shift && self.core.has_selection() {
            // Shift+click moves the end of the selection to the click, and a drag
            // from there keeps moving it.
            self.core.extend_selection(row, column, side);
            drag.started = true;
        } else if event.modifiers.shift && !mouse_reporting(self.core.mode()) {
            // With nothing selected, Shift+click selects from the cursor to the
            // click. A program that took the mouse places its own cursor, so there
            // Shift only keeps the pointer for the terminal and the drag starts
            // where the button went down, like any other.
            self.core.begin_selection_at_cursor();
            self.core.extend_selection(row, column, side);
            drag.started = true;
        } else {
            // A press only records where it was; the old highlight goes, as the
            // press starts something new.
            self.core.clear_selection();
        }
        self.drag = Some(drag);
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
        self.menu_link = self.hit_test_link(event.position).map(|link| link.url);
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

    /// Pointer motion over the grid, which a program that took the mouse is told
    /// about. A selection drag follows the pointer through
    /// [`TerminalPane::drag_moved`] instead, wherever in the window it is.
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        self.update_hovered_link(Some(event.position), &event.modifiers, cx);
        if self.drag.is_some() {
            return;
        }
        let button = self.mouse_held.unwrap_or(TerminalMouseButton::None);
        self.report_mouse(
            button,
            MouseAction::Motion,
            event.position,
            &event.modifiers,
            cx,
        );
    }

    /// The pointer moved anywhere in the window while a selection drag is held.
    ///
    /// The grid only hears its own hover, so a drag that left the grid, or the
    /// window, would stop following the pointer and never hear the release. The
    /// pane listens at the window instead (see [`TerminalPane::viewport`]) and
    /// does nothing unless a drag is held here.
    fn drag_moved(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.scrollbar_drag {
            if event.pressed_button != Some(MouseButton::Left) {
                self.scrollbar_drag = false;
                cx.notify();
            } else {
                self.scrub_scrollbar(event.position.y, cx);
            }
            return;
        }
        if self.drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The release happened where the window never heard it; the drag ends
            // where the pointer is now.
            self.finish_drag(event.position, cx);
            return;
        }
        self.drag_to(event.position, cx);
        self.start_autoscroll(cx);
    }

    /// The left button came up anywhere in the window while a drag is held.
    fn drag_released(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.scrollbar_drag {
            self.scrollbar_drag = false;
            cx.notify();
            return;
        }
        if self.drag.is_some() {
            self.finish_drag(event.position, cx);
        }
    }

    fn scrub_scrollbar(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let history = self.core.history();
        let Some(bounds) = self.viewport else {
            return;
        };
        let height = f32::from(bounds.size.height);
        if history == 0 || height <= 1.0 {
            return;
        }
        let ratio = ((f32::from(y - bounds.origin.y)) / height).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let target = ((1.0 - ratio) * history as f32).round() as usize;
        self.scroll_to_offset(target.min(history));
        if self.core.display_offset() == 0 {
            self.unseen_output = false;
        }
        cx.notify();
    }

    /// Moves the end of the drag's selection to a window position, beginning the
    /// selection once the pointer has gone far enough.
    fn drag_to(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(((row, column), side)) = self.cell_at(position) else {
            return;
        };
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.pointer = position;
        if !drag.started {
            let dx = f32::from(position.x - drag.origin.x);
            let dy = f32::from(position.y - drag.origin.y);
            let far = dx * dx + dy * dy > SELECTION_DRAG_THRESHOLD * SELECTION_DRAG_THRESHOLD;
            if !far && (row, column) == drag.anchor.0 {
                return;
            }
            drag.started = true;
            let ((anchor_row, anchor_column), anchor_side) = drag.anchor;
            let anchor_line = drag.anchor_line;
            let kind = drag.kind;
            if let Some(anchor) = anchor_line {
                self.core
                    .begin_selection_at_anchor(anchor, anchor_column, anchor_side, kind);
            } else {
                self.core
                    .begin_selection(anchor_row, anchor_column, anchor_side, kind);
            }
        }
        self.core.extend_selection(row, column, side);
        cx.notify();
    }

    /// Ends the drag at a window position.
    fn finish_drag(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.drag_to(position, cx);
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag.started && self.core.has_selection() {
            // Copying on release is what makes "select with the mouse, paste" work
            // without a second step; the copy button stays for a repeat.
            self.copy_selection(cx);
        } else {
            // A click selected nothing: no one-cell highlight claims it did, and
            // the clipboard keeps what the user put there.
            self.core.clear_selection();
        }
        cx.notify();
    }

    /// Lines one auto-scroll step moves for a pointer at window height `y`:
    /// positive back into the history when it is above the grid, negative
    /// towards the live output when it is below, zero inside. One line per row
    /// height past the edge, rounded up, at most
    /// [`SELECTION_AUTOSCROLL_MAX_LINES`].
    fn autoscroll_lines(&self, y: Pixels) -> i32 {
        let Some(bounds) = self.viewport else {
            return 0;
        };
        let height = self.cell.sanitized().height;
        let above = f32::from(bounds.origin.y - y);
        let below = f32::from(y - bounds.bottom());
        let lines = |distance: f32| {
            ((distance / height).ceil() as usize).clamp(1, SELECTION_AUTOSCROLL_MAX_LINES) as i32
        };
        if above > 0.0 {
            lines(above)
        } else if below > 0.0 {
            -lines(below)
        } else {
            0
        }
    }

    /// Starts scrolling while a selection drag holds the pointer past the top or
    /// bottom edge of the grid.
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscrolling {
            return;
        }
        let Some(drag) = self.drag.filter(|drag| drag.started) else {
            return;
        };
        if self.autoscroll_lines(drag.pointer.y) == 0 {
            return;
        }
        self.autoscrolling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(SELECTION_AUTOSCROLL_INTERVAL)
                    .await;
                let keep_going = this.update(cx, |pane, cx| pane.autoscroll_step(cx));
                match keep_going {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// One auto-scroll step: scrolls towards the pointer and extends the
    /// selection to the row now at the edge. Returns whether to keep going.
    fn autoscroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let pointer = match self.drag {
            Some(drag) if drag.started => drag.pointer,
            _ => {
                self.autoscrolling = false;
                return false;
            }
        };
        let lines = self.autoscroll_lines(pointer.y);
        if lines == 0 {
            self.autoscrolling = false;
            return false;
        }
        self.core.scroll(lines);
        if self.core.display_offset() == 0 {
            self.unseen_output = false;
        }
        self.drag_to(pointer, cx);
        cx.notify();
        true
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if matches!(event.delta, ScrollDelta::Lines(_)) {
            self.scroll_gesture_active = false;
            self.scroll_gesture_pinned = false;
        } else if matches!(event.touch_phase, gpui::TouchPhase::Started) {
            self.scroll_gesture_active = true;
            self.scroll_gesture_pinned = false;
            self.scroll_fraction = 0.0;
        }
        if self.scroll_gesture_pinned {
            return;
        }
        let scrolled = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / self.cell.sanitized().height,
        };
        // The fraction is kept rather than rounded up: a trackpad sends a gesture as
        // many deltas smaller than a line, and moving a line for each of them would
        // both make the scroll jump and turn the last, near-zero events of a gesture
        // into lines of their own — the view would go on scrolling after the fingers
        // stopped, and climb back out of the bottom it had just reached.
        self.scroll_fraction += scrolled;
        let whole = self.scroll_fraction.trunc();
        self.scroll_fraction -= whole;
        let lines = whole as i32;
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
        let before = self.core.display_offset();
        self.core.scroll(lines);
        // The pointer did not move, the text under it did: what it rests on may now
        // be a link, or no longer be one (R1).
        self.refresh_hovered_link();
        if self.core.display_offset() == 0 {
            self.unseen_output = false;
            if lines < 0 {
                self.scroll_gesture_pinned = self.scroll_gesture_active;
                self.scroll_fraction = 0.0;
            }
        }
        // A scroll the history could not take leaves nothing behind: the next event
        // starts from where the view really is instead of from where a gesture that
        // ran past an end wanted it to be.
        if self.core.display_offset() == before {
            self.scroll_fraction = 0.0;
        }
        cx.notify();
    }

    /// The right-click menu: link actions, copy/paste, search, scrollback clear,
    /// snippets and pane splits.
    fn context_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let position = self.menu?;
        let metrics = theme::metrics();
        let strings = self.locale.strings();
        let viewport = window.viewport_size();
        let can_copy = self.core.has_selection();
        let can_search = !self.alternate_screen();
        let can_clear = !self.alternate_screen();
        let can_snippets = self.state.accepts_input();
        let can_paste = if self.search_captures_keyboard() {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| !text.is_empty())
        } else if !self.state.accepts_input() {
            false
        } else {
            cx.read_from_clipboard().is_some_and(|item| {
                if item.text().is_some_and(|text| !text.is_empty()) {
                    return true;
                }
                !matches!(self.origin, PaneOrigin::Serial { .. })
                    && item
                        .into_entries()
                        .any(|entry| matches!(entry, gpui::ClipboardEntry::Image(_)))
            })
        };
        // The hint names the binding in force, like the tooltips do: a rebound key, or
        // an unbinding that handed the combination back to the program, is described as
        // it really is (R5).
        // The entries are the shared menu entry, so this menu is the same object as the
        // tab's and the file view's (R6).
        let item = |label: &'static str,
                    icon: Icon,
                    shortcut: &Option<String>,
                    enabled: bool,
                    id: &'static str| {
            components::menu_entry(label, id, Some(icon), enabled, false, metrics).when_some(
                shortcut.clone(),
                |item, shortcut| {
                    item.child(
                        components::menu_shortcut(SharedString::from(shortcut), metrics)
                            .debug_selector(move || format!("{id}.shortcut")),
                    )
                },
            )
        };
        let ux = crate::app::copy::ux_for(self.locale);
        let scrolled_back = !self.alternate_screen() && self.core.display_offset() > 0;
        let menu = components::menu_card("terminal.menu", metrics)
            .id("terminal.menu.scroll")
            .max_h(
                (viewport.height - metrics.scale(spacing::LOOSE))
                    .max(metrics.scale(components::MENU_MIN_WIDTH)),
            )
            .overflow_y_scroll()
            .when_some(self.menu_link.clone(), |menu, url| {
                let open_url = url.clone();
                let copy_url = url;
                menu.child(
                    item(
                        ux.terminal_open_link,
                        Icon::Globe,
                        &None,
                        true,
                        "terminal.menu.open_link",
                    )
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        this.open_link(&open_url, cx);
                    })),
                )
                .child(
                    item(
                        ux.terminal_copy_link,
                        Icon::Copy,
                        &None,
                        true,
                        "terminal.menu.copy_link",
                    )
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                        this.last_copy = Some(copy_url.clone());
                        this.menu = None;
                        this.menu_link = None;
                        cx.notify();
                    })),
                )
                .child(components::menu_divider(metrics))
            })
            .child(
                item(
                    strings.field_copy,
                    Icon::Copy,
                    &self.shortcuts.copy,
                    can_copy,
                    "terminal.menu.copy",
                )
                .when(can_copy, |item| {
                    item.on_click(cx.listener(|this, _event, _window, cx| {
                        this.copy_selection(cx);
                        this.menu = None;
                        this.menu_link = None;
                        cx.notify();
                    }))
                }),
            )
            .child(
                item(
                    strings.field_paste,
                    Icon::Import,
                    &self.shortcuts.paste,
                    can_paste,
                    "terminal.menu.paste",
                )
                .when(can_paste, |item| {
                    item.on_click(cx.listener(|this, _event, window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        this.paste(window, cx);
                        cx.notify();
                    }))
                }),
            )
            .child(
                item(
                    ux.terminal_select_all,
                    Icon::Layers,
                    &None,
                    true,
                    "terminal.menu.select_all",
                )
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.core.select_all();
                    this.menu = None;
                    this.menu_link = None;
                    cx.notify();
                })),
            )
            .child(components::menu_divider(metrics))
            .child(
                item(
                    strings.terminal_search,
                    Icon::Search,
                    &self.shortcuts.search,
                    can_search,
                    "terminal.menu.search",
                )
                .when(can_search, |item| {
                    item.on_click(cx.listener(|this, _event, window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        this.toggle_search(window, cx);
                    }))
                }),
            )
            .when(scrolled_back, |menu| {
                menu.child(
                    item(
                        ux.terminal_jump_bottom_short,
                        Icon::ChevronDown,
                        &None,
                        true,
                        "terminal.menu.jump_bottom",
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        this.reveal_input(cx);
                    })),
                )
            })
            .child(
                item(
                    ux.terminal_clear_scrollback,
                    Icon::Trash,
                    &None,
                    can_clear,
                    "terminal.menu.clear",
                )
                .when(can_clear, |item| {
                    item.on_click(cx.listener(|this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        this.clear_terminal_and_scrollback(cx);
                    }))
                }),
            )
            .child(components::menu_divider(metrics))
            .child(
                item(
                    ux.terminal_snippets_menu,
                    Icon::Play,
                    &None,
                    can_snippets,
                    "terminal.menu.snippets",
                )
                .when(can_snippets, |item| {
                    item.on_click(cx.listener(|this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        cx.emit(PaneRequest::OpenSnippets);
                        cx.notify();
                    }))
                }),
            )
            .child(
                item(
                    strings.pane_split_right,
                    Icon::SplitRight,
                    &self.shortcuts.split_right,
                    true,
                    "terminal.menu.split_right",
                )
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.menu = None;
                    this.menu_link = None;
                    cx.emit(PaneRequest::Split(shellune_core::SplitAxis::LeftRight));
                    cx.notify();
                })),
            )
            .child(
                item(
                    strings.pane_split_down,
                    Icon::SplitDown,
                    &self.shortcuts.split_down,
                    true,
                    "terminal.menu.split_down",
                )
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.menu = None;
                    this.menu_link = None;
                    cx.emit(PaneRequest::Split(shellune_core::SplitAxis::TopBottom));
                    cx.notify();
                })),
            )
            // Logging this terminal's output, the same three entries for a local, an
            // SSH and a serial terminal (R3).
            .child(components::menu_divider(metrics))
            .when(self.log.is_some(), |menu| {
                menu.child(
                    item(
                        ux.terminal_log_stop,
                        Icon::Stop,
                        &None,
                        true,
                        "terminal.menu.log_stop",
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        cx.emit(PaneRequest::StopLog);
                        cx.notify();
                    })),
                )
            })
            .when(self.log.is_none(), |menu| {
                menu.child(
                    item(
                        ux.terminal_log_start_plain,
                        Icon::File,
                        &None,
                        true,
                        "terminal.menu.log_plain",
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        cx.emit(PaneRequest::StartLog(LogFormat::Plain));
                        cx.notify();
                    })),
                )
                .child(
                    item(
                        ux.terminal_log_start_raw,
                        Icon::File,
                        &None,
                        true,
                        "terminal.menu.log_raw",
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.menu = None;
                        this.menu_link = None;
                        cx.emit(PaneRequest::StartLog(LogFormat::Raw));
                        cx.notify();
                    })),
                )
            });
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
                                    this.menu_link = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            gpui::anchored()
                                .position(position)
                                .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                                .child(
                                    div()
                                        .occlude()
                                        .child(components::menu_enter(menu, "terminal.menu.enter")),
                                ),
                        ),
                ),
            )
            .with_priority(3),
        )
    }

    /// How wide the bar under the grid was at its last layout - the pane's own width -
    /// when it has been laid out.
    #[must_use]
    pub fn bar_width(&self) -> Option<Pixels> {
        self.bar_room.get().width
    }

    /// Hands the pane the session's half of its bar for the next draw (R3).
    pub fn set_session_slot(&self, slot: SessionSlot) {
        *self.session_slot.borrow_mut() = Some(slot);
    }

    /// How many of the session's controls, counted from the end of the list the window
    /// handed in, the bar had no room for at its last layout (R3, R9).
    ///
    /// The window offers those in the session's menu, which always stays on the bar.
    #[must_use]
    pub fn bar_hidden_controls(&self) -> usize {
        if self.bar_has_slot.get() {
            self.bar_room.get().hidden
        } else {
            0
        }
    }

    /// Hands the pane the banner the window puts above its grid for the next draw:
    /// why the session ended, or that it is coming back (ADR-6), and the first of its
    /// controls, which Tab reaches from a terminal that no longer takes input.
    ///
    /// The banner covers nothing: the grid moves down under it and keeps its last
    /// screen, which can still be scrolled, selected and copied.
    ///
    /// The banner comes in parts stacked in order: its one line, which keeps its height,
    /// and the details it unfolded, which give way first in a short pane.
    pub fn set_status_banner(&self, banner: Vec<gpui::AnyElement>, focus: Option<FocusHandle>) {
        *self.banner_slot.borrow_mut() = Some((banner, focus));
    }

    /// The banner above the grid, when the window handed one in.
    fn status_banner(&self) -> Vec<gpui::AnyElement> {
        let slot = self.banner_slot.borrow_mut().take();
        let (banner, focus) = match slot {
            Some((banner, focus)) => (banner, focus),
            None => (Vec::new(), None),
        };
        *self.banner_focus.borrow_mut() = focus;
        banner
    }

    /// The grid size as the bar's menu states it.
    pub(crate) fn grid_label(&self) -> String {
        format!("{}\u{00D7}{}", self.grid.cols, self.grid.rows)
    }

    /// Records what the bar's layout measured, and draws again when it changed.
    fn apply_bar_room(&mut self, room: BarRoom, cx: &mut Context<Self>) {
        let old = self.bar_room.get();
        let close = |a: Option<Pixels>, b: Option<Pixels>| match (a, b) {
            (Some(a), Some(b)) => (a - b).abs() < px(LAYOUT_SLACK),
            (None, None) => true,
            _ => false,
        };
        let near = |a: Pixels, b: Pixels| (a - b).abs() < px(LAYOUT_SLACK);
        if close(old.width, room.width)
            && near(old.controls, room.controls)
            && near(old.pinned, room.pinned)
            && old.clipped == room.clipped
            && old.hidden == room.hidden
        {
            self.bar_room.set(room);
            return;
        }
        self.bar_room.set(room);
        // Measured while the window draws, where a notification is not taken: the
        // pane asks for its next draw once this one is done.
        let pane = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = pane.update(cx, |_pane, cx| cx.notify());
        });
    }

    /// The serial line own row above the bar (R21).
    ///
    /// It names the device and the settings it was opened with, carries the two send
    /// habits, and - while the session is not live - says why, what the port list holds
    /// and offers the refresh. Everything it states comes from what really happened: a
    /// list that could not be read is never drawn as "no ports", and a saved device that
    /// is absent is named as absent rather than quietly replaced by whatever else is
    /// attached.
    fn serial_strip(&self, metrics: Metrics, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let serial = self.serial.as_ref()?;
        let strings = self.locale.strings();
        let live = self.state.accepts_input();
        let muted = theme::swatch(shell_palette::TEXT_MUTED);
        let echo_on = serial.echo();
        let echo = if echo_on {
            strings.serial_echo_on
        } else {
            strings.serial_echo_off
        };
        let device_summary = format!("{} \u{00B7} {}", serial.device(), serial.settings());
        let row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .pl(metrics.scale(spacing::BASE - 2.0))
            .pr(metrics.scale(spacing::HAIRLINE))
            .py(px(spacing::HALF))
            .text_size(metrics.caption_size())
            .text_color(muted)
            .child(
                div()
                    .id("serial.device.label")
                    .min_w_0()
                    .truncate()
                    .font_family(monospace_family())
                    .tooltip(components::tooltip(SharedString::from(
                        device_summary.clone(),
                    )))
                    .debug_selector(|| "serial.device".to_owned())
                    .child(SharedString::from(device_summary)),
            )
            .child(components::divider_vertical(metrics))
            // The two send habits and the refresh are the toolbar's own quiet buttons,
            // so they answer the pointer like every other control of the bar (R6).
            .child(
                components::compact_button(
                    format!("{} {echo}", strings.serial_echo),
                    if echo_on {
                        components::Variant::Secondary
                    } else {
                        components::Variant::Ghost
                    },
                    metrics,
                )
                .h(metrics.hit_target())
                .px(metrics.scale(spacing::TIGHT))
                .id("serial.echo")
                .debug_selector(|| "serial.echo".to_owned())
                .tooltip(components::tooltip(SharedString::from(format!(
                    "{}: {echo}",
                    strings.serial_echo
                ))))
                .on_click(cx.listener(|this, _event, _window, cx| {
                    let next = this.serial.as_ref().is_none_or(|serial| !serial.echo());
                    this.set_serial_echo(next, cx);
                })),
            )
            .child(
                components::compact_button(
                    format!(
                        "{} {}",
                        strings.serial_newline,
                        serial.format().localized_label(self.locale)
                    ),
                    components::Variant::Ghost,
                    metrics,
                )
                .h(metrics.hit_target())
                .px(metrics.scale(spacing::TIGHT))
                .id("serial.newline")
                .debug_selector(|| "serial.newline".to_owned())
                .tooltip(components::tooltip(SharedString::from(format!(
                    "{}: {} ({} / CR / LF / CRLF)",
                    strings.serial_newline,
                    serial.format().localized_label(self.locale),
                    strings.serial_newline_as_typed,
                ))))
                .on_click(cx.listener(|this, _event, _window, cx| {
                    let Some(current) = this.serial.as_ref().map(SerialPane::format) else {
                        return;
                    };
                    // The ladder is the whole catalogue in one order, so pressing the
                    // control always reaches every choice and ends where it started.
                    let all = NewlineFormat::ALL;
                    let index = all
                        .iter()
                        .position(|format| *format == current)
                        .unwrap_or(0);
                    let next = all[(index + 1) % all.len()];
                    this.set_serial_format(next, cx);
                })),
            )
            .when(live, |row| {
                row.child(
                    components::compact_button(
                        strings.serial_send_break,
                        components::Variant::Ghost,
                        metrics,
                    )
                    .h(metrics.hit_target())
                    .px(metrics.scale(spacing::TIGHT))
                    .id("serial.break")
                    .debug_selector(|| "serial.break".to_owned())
                    .tooltip(components::tooltip(SharedString::from(
                        strings.serial_send_break,
                    )))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.send_serial_break(cx);
                    })),
                )
                .child(
                    components::compact_button(
                        strings.serial_disconnect,
                        components::Variant::Ghost,
                        metrics,
                    )
                    .h(metrics.hit_target())
                    .px(metrics.scale(spacing::TIGHT))
                    .id("serial.disconnect")
                    .debug_selector(|| "serial.disconnect".to_owned())
                    .tooltip(components::tooltip(SharedString::from(
                        strings.serial_disconnect,
                    )))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.disconnect_serial(cx);
                    })),
                )
            })
            .when(!live, |row| {
                row.child(
                    components::compact_button(
                        strings.serial_reconnect,
                        components::Variant::Primary,
                        metrics,
                    )
                    .h(metrics.hit_target())
                    .px(metrics.scale(spacing::TIGHT))
                    .id("serial.reconnect")
                    .debug_selector(|| "serial.reconnect".to_owned())
                    .tooltip(components::tooltip(SharedString::from(
                        strings.serial_reconnect,
                    )))
                    .on_click(cx.listener(|_this, _event, _window, cx| {
                        cx.emit(PaneRequest::SerialReconnect(None));
                    })),
                )
                .child(
                    components::compact_button(
                        strings.serial_ports_refresh,
                        components::Variant::Secondary,
                        metrics,
                    )
                    .h(metrics.hit_target())
                    .px(metrics.scale(spacing::TIGHT))
                    .id("serial.ports.refresh")
                    .debug_selector(|| "serial.ports.refresh".to_owned())
                    .tooltip(components::tooltip(SharedString::from(
                        strings.serial_ports_refresh,
                    )))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.refresh_serial_ports(cx);
                    })),
                )
            })
            .when_some(serial.note().map(str::to_owned), |row, note| {
                let tone = serial.note_tone();
                // Severity-graded dismissible pill: Warning for overflow/incomplete,
                // Danger for port errors or held drivers.
                row.child(
                    div()
                        .id("serial.note.pill")
                        .debug_selector(|| "serial.note".to_owned())
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .px(metrics.scale(spacing::TIGHT))
                        .py(px(spacing::HAIRLINE))
                        .rounded(px(radius::SMALL))
                        .bg(theme::swatch(tone.tint()))
                        .border_1()
                        .border_color(theme::faded(tone.text(), 0.35))
                        .text_color(tone.text())
                        .min_w_0()
                        .max_w(metrics.scale(420.0))
                        .tooltip(components::tooltip(SharedString::from(note.clone())))
                        .child(
                            div()
                                .id("serial.note.text")
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(note)),
                        )
                        .child(
                            components::icon_only_button(Icon::Close, metrics)
                                .h(metrics.scale(18.0))
                                .w(metrics.scale(18.0))
                                .id("serial.note.dismiss")
                                .debug_selector(|| "serial.note.dismiss".to_owned())
                                .tooltip(components::tooltip(SharedString::from(
                                    strings.serial_note_dismiss,
                                )))
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.dismiss_serial_note(cx);
                                })),
                        ),
                )
            })
            .when(!live, |row| {
                let mut row = row.child(
                    div()
                        .truncate()
                        .debug_selector(|| "serial.ports".to_owned())
                        .child(SharedString::from(ports_line(serial, self.locale))),
                );
                for (index, port) in serial.ports().ports().iter().enumerate() {
                    let path = port.path().to_owned();
                    let label = port.name().to_owned();
                    let hint = crate::app::copy::fill(
                        strings.serial_switch_port_hint,
                        &[("port", &label)],
                    );
                    let selector = format!("serial.port.{index}");
                    row = row.child(
                        components::compact_button(label, components::Variant::Ghost, metrics)
                            .h(metrics.hit_target())
                            .px(metrics.scale(spacing::TIGHT))
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector.clone())
                            .tooltip(components::tooltip(SharedString::from(hint)))
                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                cx.emit(PaneRequest::SerialReconnect(Some(path.clone())));
                            })),
                    );
                }
                row
            });
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .debug_selector(|| "serial.strip".to_owned())
                .bg(theme::swatch(shell_palette::CHROME))
                .border_t_1()
                .border_color(theme::faded(theme::swatch(shell_palette::BORDER), 0.8))
                .child(row),
        )
    }
    /// The status line under the grid, 28 high (R3, ADR-7).
    ///
    /// The left group is where the session is: its mark and target and, once the
    /// program stopped, how it stopped; it is cut with an ellipsis before anything else
    /// gives way. The middle group is what the window hands in for the session - a
    /// reconnect or a save that is waiting, the files switch, splitting - then search
    /// and the session's menu. Every other tool of the pane - the grid size, the
    /// presentation, copy and the font size - is in that menu, so the bar always reads
    /// files, split, search, more. A pane that does not carry the session's half keeps
    /// search and its own "more" menu at the right instead.
    ///
    /// A live process needs no badge: the session's half states the connection, so the
    /// state is only spelled out once the program stopped, in the tone of how it
    /// stopped.
    fn bar(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let slot = self.session_slot.borrow_mut().take();
        let (slot_left, slot_controls, slot_pinned) = match slot {
            Some(slot) => (Some(slot.left), slot.controls, slot.pinned),
            None => (None, Vec::new(), Vec::new()),
        };
        let has_controls = !slot_controls.is_empty();
        let has_slot = slot_left.is_some();
        self.bar_has_slot.set(has_slot);
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
        let bar_height = metrics.hit_target().max(metrics.scale(SHELL_BAR_HEIGHT));
        // The bar's children are the three groups and, last, a box of the bar's own
        // size; the middle group holds the session's controls at their natural width,
        // search and the session's menu.
        let weak = cx.entity().downgrade();
        let measure_bar = move |bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
            let Some(frame) = bounds.get(3) else {
                return;
            };
            let width = frame.size.width;
            let _ = weak.update(cx, |pane, cx| {
                let room = BarRoom {
                    width: Some(width),
                    ..pane.bar_room.get()
                };
                pane.apply_bar_room(room, cx);
            });
        };
        let weak = cx.entity().downgrade();
        let middle_gap = metrics.scale(spacing::HALF);
        // Every control keeps its own width whether or not it fits, so their sum is the
        // natural width however much of it is shown.
        let natural = move |bounds: &[Bounds<Pixels>]| {
            let gaps = middle_gap * bounds.len().saturating_sub(1) as f32;
            bounds
                .iter()
                .fold(gaps, |sum, child| sum + child.size.width)
        };
        // A control that does not fit wraps onto the line under the bar's one line;
        // the line is the pointer target's height, so half of it tells the two apart
        // whatever the controls' own heights are.
        let line = metrics.hit_target();
        let measure_controls =
            move |bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
                let controls = natural(&bounds);
                let top = bounds
                    .iter()
                    .map(|child| child.origin.y)
                    .fold(None, |top: Option<Pixels>, y| {
                        Some(top.map_or(y, |top| top.min(y)))
                    });
                let wrapped = top.map_or(0, |top| {
                    bounds
                        .iter()
                        .filter(|child| child.origin.y > top + line / 2.0)
                        .count()
                });
                let _ = weak.update(cx, |pane, cx| {
                    let room = BarRoom {
                        controls,
                        wrapped,
                        count: bounds.len(),
                        first_right: bounds.first().map(|first| first.right()),
                        ..pane.bar_room.get()
                    };
                    pane.apply_bar_room(room, cx);
                });
            };
        let weak = cx.entity().downgrade();
        let measure_middle =
            move |bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
                let pinned = if has_controls {
                    bounds.get(1..).unwrap_or_default()
                } else {
                    &bounds[..]
                };
                let pinned = natural(pinned);
                let group = has_controls.then(|| bounds.first().copied()).flatten();
                let _ = weak.update(cx, |pane, cx| {
                    let previous = pane.bar_room.get();
                    let room = match group {
                        // The controls were measured in this frame, just before.
                        Some(group) => {
                            let clipped = previous
                                .first_right
                                .is_some_and(|right| right > group.right() + px(LAYOUT_SLACK));
                            BarRoom {
                                pinned,
                                clipped,
                                hidden: if clipped {
                                    previous.count
                                } else {
                                    previous.wrapped
                                },
                                ..previous
                            }
                        }
                        None => BarRoom {
                            pinned,
                            controls: px(0.0),
                            wrapped: 0,
                            count: 0,
                            first_right: None,
                            clipped: false,
                            hidden: 0,
                            ..previous
                        },
                    };
                    pane.apply_bar_room(room, cx);
                });
            };

        let left = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            // Where the session is keeps at least its mark and a letter or two: in a pane
            // too narrow for everything, the session's controls give way before it.
            .min_w(if has_slot {
                metrics.scale(SESSION_SLOT_MIN_WIDTH)
            } else {
                px(0.0)
            })
            .flex_1()
            .overflow_hidden()
            .debug_selector(|| "terminal.bar.left".to_owned())
            .children(slot_left)
            // The program is named on the session's half and the tab; down here it is
            // only repeated once it stopped, beside how it stopped.
            .when(!live, |row| {
                // The program that stopped is named here; its state is only spelled out
                // when this bar is the session's only one. A pane that carries the
                // session's half already has the session's own status beside the target,
                // and saying it twice is what let one session read as two different
                // states (R17).
                let row = row.child(
                    div()
                        .truncate()
                        .debug_selector(|| "terminal.shell".to_owned())
                        .child(SharedString::from(self.origin.label().to_owned())),
                );
                if has_slot {
                    row
                } else {
                    row.child(components::chip(
                        self.state.label(self.locale),
                        tone,
                        metrics,
                    ))
                }
            })
            .when(self.input_busy, |row| {
                row.child(components::chip(
                    match self.locale {
                        Locale::Chinese => "输入缓冲已满，本次输入未发送",
                        Locale::English => "Input buffer full; this input was not sent",
                    },
                    Tone::Warning,
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
            // Said under a running shell, not only after it stopped: a shell started
            // without the login environment or in a replaced locale still runs, and this
            // is the only place the user learns why a tool is missing or Chinese looks
            // wrong (R14, R15).
            .when_some(self.environment_note(), |row, note| {
                // The whole sentence is in the tooltip: the bar is one line, and a note
                // the user cannot finish reading is not a notice.
                row.child(
                    div()
                        .id("terminal.environment")
                        .truncate()
                        .debug_selector(|| "terminal.environment".to_owned())
                        .tooltip(components::tooltip(note.clone()))
                        .child(note),
                )
            });

        // The session's controls keep their natural width while there is room; in a pane
        // too narrow even for the folded tools this is the group that gives way, so search
        // and the menu that holds the rest are never the ones cut off. It gives way a
        // whole control at a time, from its end: one that does not fit wraps onto a
        // second line, which the group's one-line height hides, instead of being cut
        // through. The session's menu is outside that group and never gives way: it
        // offers every control the bar has no room for (R3, R9).
        // A group narrower than its first control would cut it through; it is then
        // laid out as before, so the measure stays put, but not drawn, and the menu
        // offers all of it.
        let clipped = self.bar_room.get().clipped;
        let controls = has_controls.then(|| {
            div()
                .on_children_prepainted(measure_controls)
                .when(clipped, |group| group.invisible())
                .flex()
                .flex_wrap()
                .flex_shrink()
                .min_w_0()
                .h(metrics.hit_target())
                .overflow_hidden()
                .items_center()
                .gap(middle_gap)
                .debug_selector(|| "terminal.bar.controls".to_owned())
                .children(slot_controls)
        });
        let middle = div()
            .on_children_prepainted(measure_middle)
            .flex()
            .flex_shrink()
            // The group shrinks down to its menu and no further.
            .min_w(self.bar_room.get().pinned)
            .h(metrics.hit_target())
            .overflow_hidden()
            .items_center()
            .gap(middle_gap)
            .debug_selector(|| "terminal.bar.middle".to_owned())
            .children(controls)
            // Search stands between the session's controls and its menu, so the bar
            // reads files, split, search, more (ADR-7); like the menu it never gives way.
            .when(has_slot, |middle| {
                middle.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .child(self.search_button(metrics, self.locale.strings(), cx)),
                )
            })
            .children({
                let has_hidden = self.bar_room.get().hidden > 0;
                slot_pinned
                    .into_iter()
                    .enumerate()
                    .map(move |(index, pinned)| {
                        div()
                            .relative()
                            .flex()
                            .flex_none()
                            .items_center()
                            .child(pinned)
                            .when(index == 0 && has_hidden, |slot| {
                                slot.child(
                                    div()
                                        .absolute()
                                        .top(metrics.scale(spacing::HALF))
                                        .right(metrics.scale(spacing::HALF))
                                        .size(metrics.scale(spacing::HAIRLINE))
                                        .rounded_full()
                                        .bg(theme::swatch(shell_palette::ACCENT))
                                        .debug_selector(|| "session.more.overflow".to_owned()),
                                )
                            })
                    })
            });

        div()
            .on_children_prepainted(measure_bar)
            .id("terminal.bar")
            .debug_selector(|| "terminal.bar".to_owned())
            .relative()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            // At least the bar's height; taller when the window's text is larger, so the
            // session's half it carries is never cut off at its bottom.
            .min_h(bar_height)
            .flex_none()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .pl(metrics.scale(spacing::BASE - 2.0))
            .pr(metrics.scale(spacing::HAIRLINE))
            .bg(theme::swatch(shell_palette::CHROME))
            .border_t_1()
            .border_color(theme::faded(theme::swatch(shell_palette::BORDER), 0.8))
            .text_size(metrics.caption_size())
            .text_color(muted)
            .child(left)
            .child(middle)
            .child(self.bar_tools(has_slot, metrics, cx))
            .child(div().absolute().inset_0())
    }

    /// The bar's right group: search and the "more" menu that holds every other tool of
    /// this pane (ADR-7).
    ///
    /// A pane that carries the session's half has one menu, the session's: its search
    /// stands before that menu in the middle group and the tools are in the session's
    /// menu, so this group is empty. The group is drawn either way, so the bar always
    /// has the same children for its measure.
    fn bar_tools(&self, has_slot: bool, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let strings = self.locale.strings();
        let target = metrics.hit_target();
        let ux = crate::app::copy::ux_for(self.locale);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::HALF))
            .children(self.log_badge(metrics))
            .debug_selector(|| "terminal.bar.tools".to_owned())
            .when(!has_slot, |row| {
                row.child(self.search_button(metrics, strings, cx)).child(
                    components::icon_only_button(Icon::More, metrics)
                        .w(target)
                        .h(target)
                        .when(self.more_menu.is_some(), |button| {
                            button.bg(theme::swatch(shell_palette::SURFACE_SELECTED))
                        })
                        .id("terminal.more")
                        .debug_selector(|| "terminal.more".to_owned())
                        .tooltip(components::tooltip(ux.terminal_more))
                        // Opened on the press, like the session menu, so the grid keeps
                        // the keyboard and Escape still closes it.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                                cx.stop_propagation();
                                this.menu = None;
                                this.more_menu = if this.more_menu.is_some() {
                                    None
                                } else {
                                    Some(event.position)
                                };
                                cx.notify();
                            }),
                        ),
                )
            })
    }

    /// Whether the bar's "more" menu is open, for a test.
    #[cfg(test)]
    pub fn more_menu_open_for_test(&self) -> bool {
        self.more_menu.is_some()
    }

    /// "Reset font size (14px)", in the pane's language.
    fn font_reset_label(&self) -> String {
        crate::app::copy::fill(
            crate::app::copy::ux_for(self.locale).terminal_font_reset,
            &[(
                "size",
                &format!("{:.0}", shellune_terminal::DEFAULT_FONT_SIZE),
            )],
        )
    }

    /// The entries of this pane's own tools for a menu (R3, ADR-7): the grid size,
    /// the presentation switch, copy and the font size, in that order.
    ///
    /// The bar keeps only search and "more" on its line, so every other tool is here:
    /// in the pane's own "more" menu, or in the session's menu when the pane carries the
    /// session's half of the bar. Each keeps its own action and wording: the font steps
    /// leave the menu open so a size can be stepped more than once, the grid size is
    /// stated rather than pressed, and copy and the presentation switch close the menu
    /// like any other command, through `closer`.
    pub fn tool_entries(
        &self,
        metrics: Metrics,
        closer: MenuCloser,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let strings = self.locale.strings();
        let ux = crate::app::copy::ux_for(self.locale);
        let can_copy = self.core.has_selection();
        let integrated = self.core.presentation() == TerminalMode::Integrated;
        let font_changed =
            (self.font_size - shellune_terminal::DEFAULT_FONT_SIZE).abs() >= f32::EPSILON;
        // What an entry carries at its end: the value or the key it stands for.
        let hint = |entry: gpui::Stateful<gpui::Div>, text: String| {
            entry
                .child(div().flex_1().min_w(metrics.scale(spacing::TIGHT)))
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                        .font_family(monospace_family())
                        .child(SharedString::from(text)),
                )
        };
        let mut entries: Vec<gpui::AnyElement> = Vec::new();
        entries.push(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .h(metrics.compact_control_height())
                .px(metrics.scale(spacing::TIGHT + 2.0))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(shell_palette::TEXT_SECONDARY))
                .debug_selector(|| "terminal.more.size".to_owned())
                .child(crate::app::icons::icon(
                    Icon::Maximize,
                    metrics.small_icon_size(),
                    theme::swatch(shell_palette::TEXT_MUTED),
                ))
                .child(ux.terminal_grid_size)
                .child(div().flex_1().min_w(metrics.scale(spacing::TIGHT)))
                .child(
                    div()
                        .flex_none()
                        .font_family(monospace_family())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                        .child(SharedString::from(self.grid_label())),
                )
                .into_any_element(),
        );
        let close = closer.clone();
        entries.push(
            hint(
                components::menu_entry(
                    strings.terminal_mode_blocks,
                    "terminal.more.mode",
                    Some(Icon::Layers),
                    true,
                    false,
                    metrics,
                ),
                if integrated {
                    "\u{2713}".to_owned()
                } else {
                    String::new()
                },
            )
            .on_click(cx.listener(move |this, _event, window, cx| {
                close.close(this, window, cx);
                this.set_presentation(
                    if integrated {
                        TerminalMode::Classic
                    } else {
                        TerminalMode::Integrated
                    },
                    cx,
                );
                cx.notify();
            }))
            .into_any_element(),
        );
        let copy = hint(
            components::menu_entry(
                strings.terminal_copy,
                "terminal.more.copy",
                Some(Icon::Copy),
                can_copy,
                false,
                metrics,
            ),
            self.shortcuts.copy.clone().unwrap_or_default(),
        );
        let close = closer;
        entries.push(if can_copy {
            copy.on_click(cx.listener(move |this, _event, window, cx| {
                close.close(this, window, cx);
                this.copy_selection(cx);
                cx.notify();
            }))
            .into_any_element()
        } else {
            copy.into_any_element()
        });
        let reset = hint(
            components::menu_entry(
                self.font_reset_label(),
                "terminal.more.font.reset",
                None,
                font_changed,
                false,
                metrics,
            ),
            format!("{}px", self.font_size),
        );
        entries.push(components::menu_divider(metrics).into_any_element());
        entries.push(
            hint(
                components::menu_entry(
                    strings.terminal_smaller,
                    "terminal.more.font.decrease",
                    Some(Icon::TextSize),
                    true,
                    false,
                    metrics,
                ),
                "A\u{2212}".to_owned(),
            )
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.step_font_size(false, cx);
            }))
            .into_any_element(),
        );
        entries.push(
            hint(
                components::menu_entry(
                    strings.terminal_larger,
                    "terminal.more.font.increase",
                    Some(Icon::TextSize),
                    true,
                    false,
                    metrics,
                ),
                "A+".to_owned(),
            )
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.step_font_size(true, cx);
            }))
            .into_any_element(),
        );
        entries.push(if font_changed {
            reset
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.set_font_size(shellune_terminal::DEFAULT_FONT_SIZE, cx);
                }))
                .into_any_element()
        } else {
            reset.into_any_element()
        });
        entries
    }

    /// The bar's own "more" menu, in a pane that does not carry the session's half:
    /// every tool of the pane but search (ADR-7).
    fn more_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let position = self.more_menu?;
        if self.bar_has_slot.get() {
            return None;
        }
        let metrics = theme::metrics();
        let viewport = window.viewport_size();
        let menu = components::menu_card("terminal.more.menu", metrics)
            .id("terminal.more.menu.scroll")
            .max_w(viewport.width - metrics.scale(spacing::TIGHT) * 2.0)
            .max_h(
                (viewport.height - metrics.scale(spacing::LOOSE))
                    .max(metrics.scale(components::MENU_MIN_WIDTH)),
            )
            .overflow_y_scroll()
            .children(self.tool_entries(metrics, MenuCloser::Own, cx));
        Some(
            gpui::deferred(
                gpui::anchored().position(gpui::Point::default()).child(
                    div()
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .child(
                            div()
                                .id("terminal.more.backdrop")
                                .absolute()
                                .inset_0()
                                .occlude()
                                .on_any_mouse_down(cx.listener(|this, _event, _window, cx| {
                                    this.more_menu = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            // Above the button that opened it, its right edge at the
                            // press: the bar is the bottom edge of the pane.
                            gpui::anchored()
                                .position(position)
                                .anchor(gpui::Corner::BottomRight)
                                .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                                .child(theme::enter(
                                    div()
                                        .occlude()
                                        .pb(metrics.scale(spacing::HAIRLINE))
                                        .child(menu),
                                    "terminal.more.enter",
                                    theme::Enter::Rise,
                                )),
                        ),
                ),
            )
            .with_priority(3),
        )
    }

    /// One row of the grid, drawn in three layers, every piece at its own column:
    /// the backgrounds, the cursor, then the text and the drawn shapes on top.
    ///
    /// Positions come from the column, never from the width of the glyphs drawn
    /// before: a character from a fallback face (a frame line, an icon, a CJK
    /// character) is drawn in its cell and cannot push the rest of the row off its
    /// columns.
    /// Lights the cursor and keeps it lit for one blink: it was just typed at or
    /// written past.
    fn wake_cursor(&mut self) {
        self.cursor_blink.dark = false;
        self.cursor_blink.lit_until = Some(std::time::Instant::now() + CURSOR_BLINK);
    }

    /// Starts the blink while the pane holds the keyboard; the timer stops on its own
    /// once the pane loses it.
    fn drive_cursor_blink(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.cursor_blink.focused = focused;
        if !focused {
            self.cursor_blink.dark = false;
            return;
        }
        if self.cursor_blink.running || !theme::motion_allowed() {
            return;
        }
        self.cursor_blink.running = true;
        cx.spawn(async move |pane, cx| {
            loop {
                cx.background_executor().timer(CURSOR_BLINK).await;
                let keep = pane
                    .update(cx, |pane, cx| {
                        let blink = &mut pane.cursor_blink;
                        if !blink.focused || !theme::motion_allowed() {
                            blink.running = false;
                            blink.dark = false;
                            cx.notify();
                            return false;
                        }
                        let held = blink
                            .lit_until
                            .is_some_and(|until| std::time::Instant::now() < until);
                        blink.dark = !held && !blink.dark;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

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
        // In the dark half of its blink the focused cursor is not drawn, and the
        // character under a block cursor takes its own colours again.
        let blink_dark = focused && self.cursor_blink.dark;
        let cursor = snapshot.cursor.filter(|cursor| {
            cursor.row == index && cursor.shape != CursorShape::Hidden && !blink_dark
        });
        // A focused block cursor is solid, and the character under it is drawn in
        // the canvas colour so it stays legible.
        let solid_cursor = cursor
            .filter(|cursor| focused && cursor.shape == CursorShape::Block)
            .map(|cursor| cursor.column);
        let x = |column: usize| px(column as f32 * width);
        let search_marks = self.row_search_matches(index, &row.cells);

        // Backgrounds: one rectangle per stretch of cells with the same colour.
        let mut column = 0;
        let mut stretch: Option<(usize, usize, gpui::Rgba, Option<bool>)> = None;
        let mut backgrounds = Vec::new();
        for (cell_index, rendered) in row.cells.iter().enumerate() {
            let span = rendered.width.max(1);
            let search_hit = if rendered.selected {
                None
            } else {
                search_marks.get(cell_index).copied().flatten()
            };
            let background = match search_hit {
                Some(true) => Some(theme::swatch(shell_palette::ACCENT)),
                Some(false) => Some(Tone::Warning.chip_background()),
                None => cell_background(rendered, canvas_color),
            };
            match (&mut stretch, background) {
                (Some((_, end, color, prev_hit)), Some(next))
                    if *color == next && *prev_hit == search_hit && *end == column =>
                {
                    *end = column + span;
                }
                (_, next) => {
                    backgrounds.extend(stretch.take());
                    stretch = next.map(|color| (column, column + span, color, search_hit));
                }
            }
            column += span;
        }
        backgrounds.extend(stretch);
        if backgrounds.len() > 1 && search_marks.is_empty() {
            // Colour bands have no layout or input of their own. Paint them in
            // a single row element, behind the cursor and the text runs.
            element = element.child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        for (start, end, color, _) in backgrounds {
                            let left = px(start as f32 * width);
                            let right = px(end as f32 * width);
                            let band = Bounds::new(
                                point(bounds.origin.x + left, bounds.origin.y),
                                gpui::size(right - left, bounds.size.height),
                            );
                            window.paint_quad(gpui::fill(band, color));
                        }
                    },
                )
                .absolute()
                .inset_0(),
            );
        } else {
            for (start, end, color, search_hit) in backgrounds {
                let bg = div()
                    .absolute()
                    .top_0()
                    .left(x(start))
                    .w(x(end - start))
                    .h(px(height))
                    .bg(color)
                    .when(search_hit == Some(false), |box_div| {
                        box_div
                            .border_b_1()
                            .border_color(Tone::Warning.text())
                            .debug_selector(|| "terminal.search.match".to_owned())
                    })
                    .when(search_hit == Some(true), |box_div| {
                        box_div.debug_selector(|| "terminal.search.match.current".to_owned())
                    });
                element = element.child(bg);
            }
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
        // Backgrounds, selection and the cursor have already been placed. Plain
        // spaces after the final glyph need neither contrast calculations nor shaping.
        let glyph_cells = row
            .cells
            .iter()
            .rposition(|cell| {
                cell.text != " " || cell.attributes.underline || cell.attributes.strikethrough
            })
            .map_or(0, |last| last + 1);
        let mut last_ink = None;
        for (cell_index, rendered) in row.cells[..glyph_cells].iter().enumerate() {
            let span = rendered.width.max(1);
            let search_hit = if rendered.selected {
                None
            } else {
                search_marks.get(cell_index).copied().flatten()
            };
            let background = match search_hit {
                Some(true) => theme::swatch(shell_palette::ACCENT),
                Some(false) => Tone::Warning.chip_background(),
                None => cell_background(rendered, canvas_color).unwrap_or(canvas_color),
            };
            let ink = if solid_cursor == Some(column) {
                canvas_color
            } else if search_hit == Some(true) {
                theme::swatch(shell_palette::ON_ACCENT)
            } else {
                let key = (rendered.foreground, background, rendered.attributes.dim);
                match last_ink {
                    Some((previous, ink)) if previous == key => ink,
                    _ => {
                        let value = ink(color_of(key.0), background, key.2);
                        last_ink = Some((key, value));
                        value
                    }
                }
            };
            let hovered_link_cell = self.hovered_link.as_ref().is_some_and(|link| {
                link.spans
                    .iter()
                    .any(|(row, columns)| *row == index && columns.contains(&column))
            });
            let style = TextStyle {
                ink,
                bold: rendered.attributes.bold,
                italic: rendered.attributes.italic,
                underline: rendered.attributes.underline || hovered_link_cell,
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
                let ascii = single && first.is_ascii() && span == 1;
                let continues = run.as_mut().filter(|current| {
                    current.style == style
                        && current.end == column
                        && current.glyphs.is_ascii() == ascii
                });
                if let Some(current) = continues {
                    current.glyphs.push(&rendered.text, span);
                    current.end = column + span;
                } else {
                    flush_text(&mut run, &mut pieces, cell, self.font_size, index);
                    if !blank || style.underline || style.strike {
                        let glyphs = if ascii {
                            RunGlyphs::Ascii(rendered.text.clone())
                        } else {
                            RunGlyphs::Centered(vec![(
                                SharedString::from(rendered.text.clone()),
                                span,
                            )])
                        };
                        run = Some(TextRun {
                            glyphs,
                            style,
                            start: column,
                            end: column + span,
                        });
                    }
                }
            }
            column += span;
        }
        flush_text(&mut run, &mut pieces, cell, self.font_size, index);
        flush_shapes(&mut shapes, &mut pieces, cell, stroke, index);
        element.children(pieces)
    }

    /// Maps active search hits on `row_index` to per-cell highlight states (`Some(true)`
    /// for the current match, `Some(false)` for other matches on screen).
    fn row_search_matches(&self, row_index: usize, cells: &[RenderedCell]) -> Vec<Option<bool>> {
        if !self.search_open || self.search_hits.is_empty() || self.alternate_screen() {
            return Vec::new();
        }
        let grid_line = i32::try_from(row_index).unwrap_or(0)
            - i32::try_from(self.core.display_offset()).unwrap_or(0);
        let mut marks = vec![None; cells.len()];
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(cells.len());
        let mut byte_pos = 0;
        for cell in cells {
            let len = if cell.text.is_empty() {
                1
            } else {
                cell.text.len()
            };
            ranges.push((byte_pos, byte_pos + len));
            byte_pos += len;
        }
        for (hit_index, hit) in self.search_hits.iter().enumerate() {
            if hit.line != grid_line || hit.length == 0 {
                continue;
            }
            let hit_start = hit.column;
            let hit_end = hit.column.saturating_add(hit.length);
            let is_current = hit_index == self.search_at;
            for (cell_index, &(start, end)) in ranges.iter().enumerate() {
                if start < hit_end && end > hit_start {
                    marks[cell_index] = match marks[cell_index] {
                        Some(true) => Some(true),
                        _ => Some(is_current),
                    };
                }
            }
        }
        marks
    }

    fn history_completion(&self) -> Option<&str> {
        if !self.state.accepts_input()
            || matches!(self.origin, PaneOrigin::Serial { .. })
            || self.marked.is_some()
            || self.search_open
            || self.pending_paste.is_some()
            || self.menu.is_some()
            || self.more_menu.is_some()
        {
            return None;
        }
        self.core.history_completion()
    }

    fn composition_overlay(&self, snapshot: &Snapshot) -> Option<gpui::Div> {
        if self.search_captures_keyboard() {
            return None;
        }
        let marked = self.marked.as_ref()?;
        let cell = self.cell.sanitized();
        let cursor = snapshot.cursor?;
        Some(
            div()
                .absolute()
                .left(px(cursor.column as f32 * cell.width))
                .top(px(cursor.row as f32 * cell.height))
                .px(px(spacing::HALF))
                // Opaque, so the cells under the composition do not show through it.
                .bg(theme::swatch_over(
                    shell_palette::SURFACE_SELECTED,
                    shell_palette::SURFACE,
                ))
                .border_1()
                .border_color(theme::swatch(shell_palette::ACCENT))
                .text_size(px(self.font_size))
                .line_height(px(cell.height))
                .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                .debug_selector(|| "terminal.composition".to_owned())
                .child(SharedString::from(marked.clone())),
        )
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

    /// The blocks the list shows for the current query, filter and switches.
    ///
    /// A query that cannot be read leaves no rows: the bar says why, so the list
    /// never pretends the terminal holds nothing.
    fn listed_blocks(&self) -> Vec<CommandBlock> {
        self.core
            .search_blocks(&self.search_query, self.block_filter, self.search_options)
            .unwrap_or_default()
    }

    fn search_dodges_bottom(&self) -> bool {
        self.search_open
            && !self.core.shows_command_blocks()
            && self.search_hits.get(self.search_at).is_some_and(|hit| {
                let vrow = hit.line + i32::try_from(self.core.display_offset()).unwrap_or(0);
                vrow >= i32::try_from(self.grid.rows.saturating_sub(3)).unwrap_or(0)
            })
    }

    /// The panel under the grid: the find bar and the command blocks (R1).
    ///
    /// The bar and the block list read one query, so a search means the same thing
    /// in both: the numbers come from the real scrollback, the rows from the blocks
    /// whose command or output matches with the same switches.
    fn block_overlay(
        &self,
        metrics: Metrics,
        bar_height: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let strings = self.locale.strings();
        let blocks = self.listed_blocks();
        let dodge_bottom = self.search_dodges_bottom();
        let bottom_inset = if self.focus_mode.get() {
            metrics.hit_target() + metrics.scale(spacing::BASE)
        } else {
            bar_height
        };
        let mut panel = div()
            .absolute()
            .when(dodge_bottom, |panel| panel.top(px(0.0)).border_b_1())
            .when(!dodge_bottom, |panel| {
                panel.bottom(bottom_inset).border_t_1()
            })
            .left(px(0.0))
            .right(px(0.0))
            // The bar grows with the text; the panel stops at a height that still
            // leaves the grid readable behind it and scrolls inside itself.
            .max_h(
                metrics
                    .scale(SEARCH_PANEL_MAX_HEIGHT)
                    .clamp(px(SEARCH_PANEL_MAX_HEIGHT), px(SEARCH_PANEL_MAX_HEIGHT_CAP)),
            )
            .flex()
            .flex_col()
            .id("terminal.blocks")
            .overflow_y_scroll()
            .p(metrics.scale(spacing::TIGHT))
            .gap(metrics.scale(spacing::HAIRLINE))
            .bg(theme::swatch(shell_palette::CHROME))
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
        let ux = crate::app::copy::ux_for(self.locale);
        let focused = self.search_focused;
        let inline_marked = if self.search_captures_keyboard() {
            self.marked.as_deref().filter(|text| !text.is_empty())
        } else {
            None
        };
        let empty_query = self.search_query.is_empty() && inline_marked.is_none();
        let cursor = self.clamp_char_boundary(self.search_cursor);
        let selection = if inline_marked.is_none() {
            self.normalized_search_selection()
        } else {
            None
        };
        let before = &self.search_query[..cursor];
        let after = &self.search_query[cursor..];
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
                    .min_w_0()
                    .debug_selector(|| "terminal.search.position".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(shell_palette::TEXT_SECONDARY))
                    .child(SharedString::from(format!("{position} / {total}"))),
            ),
            SearchNote::Empty => Some(
                div()
                    .min_w_0()
                    .debug_selector(|| "terminal.search.empty".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                    .child(SharedString::from(strings.terminal_search_empty.to_owned())),
            ),
            SearchNote::Invalid(reason) => Some(
                div()
                    .min_w_0()
                    .debug_selector(|| "terminal.search.error".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Danger.text())
                    .child(SharedString::from(format!(
                        "{}: {reason}",
                        strings.terminal_search_invalid
                    ))),
            ),
        };

        let accent = theme::swatch(shell_palette::ACCENT);
        // The placeholder is a sentence. A field that shrinks to a few letters and
        // then truncates cuts it through a word ("Search this te"), and the pane
        // has no other place that text is written (R4).
        let caption = metrics.caption_size();
        let proportional =
            crate::app::components::reserved_text_width(strings.terminal_search, caption);
        let mono = px(strings.terminal_search.chars().count() as f32
            * f32::from(caption)
            * shellune_terminal::MONOSPACE_ADVANCE_RATIO);
        let query_text_floor = proportional.max(mono);
        let query_field_floor = query_text_floor
            + metrics.scale(spacing::HAIRLINE) * 2.0
            + px(components::HAIRLINE_WIDTH * 2.0);
        let query_content = if empty_query {
            div()
                .flex_1()
                .min_w(query_text_floor)
                .whitespace_normal()
                .debug_selector(|| "terminal.search.query".to_owned())
                .font_family(monospace_family())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                .child(SharedString::from(strings.terminal_search.to_owned()))
        } else if let Some(sel) = selection {
            let sel_before = &self.search_query[..sel.start];
            let sel_text = &self.search_query[sel.clone()];
            let sel_after = &self.search_query[sel.end..];
            div()
                .flex()
                .items_center()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .debug_selector(|| "terminal.search.query".to_owned())
                .font_family(monospace_family())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                .when(!sel_before.is_empty(), |row| {
                    row.child(SharedString::from(sel_before.to_owned()))
                })
                .child(
                    div()
                        .flex_none()
                        .rounded(metrics.scale(spacing::HALF))
                        .bg(theme::swatch_over(
                            shell_palette::ACCENT_TINT,
                            shell_palette::SURFACE,
                        ))
                        .text_color(theme::swatch(shell_palette::ACCENT_ON_TINT))
                        .debug_selector(|| "terminal.search.selection".to_owned())
                        .child(SharedString::from(sel_text.to_owned())),
                )
                .when(focused, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .w(px(CARET_WIDTH))
                            .h(metrics.caption_size())
                            .bg(accent)
                            .debug_selector(|| "terminal.search.caret".to_owned()),
                    )
                })
                .when(!sel_after.is_empty(), |row| {
                    row.child(SharedString::from(sel_after.to_owned()))
                })
        } else {
            div()
                .flex()
                .items_center()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .debug_selector(|| "terminal.search.query".to_owned())
                .font_family(monospace_family())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                .when(!before.is_empty(), |row| {
                    row.child(SharedString::from(before.to_owned()))
                })
                .when_some(inline_marked, |row, marked| {
                    row.child(
                        div()
                            .flex_none()
                            .px(px(spacing::HALF))
                            .bg(theme::swatch_over(
                                shell_palette::SURFACE_SELECTED,
                                shell_palette::SURFACE,
                            ))
                            .border_b_1()
                            .border_color(accent)
                            .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                            .debug_selector(|| "terminal.search.composition".to_owned())
                            .child(SharedString::from(marked.to_owned())),
                    )
                })
                .when(focused, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .w(px(CARET_WIDTH))
                            .h(metrics.caption_size())
                            .bg(accent)
                            .debug_selector(|| "terminal.search.caret".to_owned()),
                    )
                })
                .when(!after.is_empty(), |row| {
                    row.child(SharedString::from(after.to_owned()))
                })
        };
        let weak_field = cx.entity().downgrade();
        let measure_search_text =
            move |bounds: Vec<Bounds<Pixels>>, _window: &mut Window, app: &mut App| {
                let first = bounds.first().copied();
                let _ = weak_field.update(app, |pane, _cx| {
                    pane.search_text_bounds.set(first);
                });
            };
        let has_query_to_clear = !self.search_query.is_empty();
        let mut bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            // The find bar is the densest bar of the terminal: its quiet buttons keep a
            // narrow padding and gap, so a pane of a split keeps the controls on two
            // lines (R4, token v2's 28 px compact height).
            .gap(metrics.scale(spacing::NARROW))
            .debug_selector(|| "terminal.search.bar".to_owned())
            // The field the keys are going to says so, and says it twice: an accent
            // border with a soft ring while the bar holds the keyboard, a neutral one
            // once the terminal has it back, and the hint beside it names where the
            // keys go in words as well (R4).
            .child(
                div()
                    .on_children_prepainted(measure_search_text)
                    .id("terminal.search.field")
                    .flex()
                    .items_center()
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if this.search_query.is_empty() {
                                this.search_selection = None;
                                this.search_cursor = 0;
                            } else if event.click_count >= 2 {
                                let len = this.search_query.len();
                                this.search_selection = Some(0..len);
                                this.search_cursor = len;
                            } else if let Some(bounds) = this.search_text_bounds.get() {
                                let run = gpui::TextRun {
                                    len: this.search_query.len(),
                                    font: gpui::font(monospace_family()),
                                    color: gpui::Hsla::default(),
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let line = window.text_system().shape_line(
                                    SharedString::from(this.search_query.clone()),
                                    metrics.caption_size(),
                                    &[run],
                                    None,
                                );
                                let rel_x = (event.position.x - bounds.left()).max(px(0.0));
                                let idx = line.closest_index_for_x(rel_x);
                                this.search_selection = None;
                                this.search_cursor = this.clamp_char_boundary(idx);
                            } else {
                                this.search_selection = None;
                                this.search_cursor = this.search_query.len();
                            }
                            this.search_focused = true;
                            window.focus(&this.focus);
                            cx.notify();
                        }),
                    )
                    // The field takes the line's spare room up to a readable cap, so a
                    // query is not squeezed into the width of its placeholder while the
                    // controls sit far away. An empty field never goes below its
                    // placeholder, so that sentence is not cut through a word (R4).
                    .flex_1()
                    .min_w(if empty_query {
                        query_field_floor
                    } else {
                        metrics.scale(SEARCH_QUERY_MIN_WIDTH)
                    })
                    .max_w(metrics.scale(SEARCH_FIELD_MAX_WIDTH).max(query_field_floor))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(shell_palette::SURFACE))
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
                            spread_radius: px(SEARCH_FOCUS_HALO),
                        }])
                    })
                    .pl(metrics.scale(spacing::HAIRLINE))
                    .pr(if has_query_to_clear {
                        px(0.0)
                    } else {
                        metrics.scale(spacing::HAIRLINE)
                    })
                    .debug_selector(move || {
                        if focused {
                            "terminal.search.keyboard.bar".to_owned()
                        } else {
                            "terminal.search.keyboard.terminal".to_owned()
                        }
                    })
                    .child(query_content)
                    .when(has_query_to_clear, |field| {
                        field.child(
                            components::icon_only_button(Icon::Close, metrics)
                                .size(metrics.hit_target())
                                .id("terminal.search.clear")
                                .debug_selector(|| "terminal.search.clear".to_owned())
                                .tooltip(components::tooltip(ux.field_clear))
                                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    cx.stop_propagation();
                                    this.clear_search_query(window, cx);
                                })),
                        )
                    }),
            );
        // The one thing the reader needs next to the field: which key ends the search, or
        // - once the terminal has the keyboard again - where their keys go. The rest of the
        // key map is in the tooltips of the controls it belongs to, so the bar is not a
        // line of instructions.
        //
        // The line may be narrower than its words: in a pane of a split tab it takes its own
        // line and wraps there, and the buttons stay whole (R4).
        let hint = div()
            .min_w_0()
            .flex_1()
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
            ));
        let with_key =
            |label: &'static str, key: &'static str| SharedString::from(format!("{label} ({key})"));
        // Two arrows, then the switches, then the hint, then the close: the arrows are the
        // verbs of a search, so they stand next to the count they move.
        bar = bar.when_some(note, |bar, note| bar.child(note)).child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .child(
                    components::icon_only_button(Icon::ChevronUp, metrics)
                        .id("terminal.search.previous")
                        .debug_selector(|| "terminal.search.previous".to_owned())
                        .tooltip(components::tooltip(with_key(
                            strings.terminal_search_previous,
                            "Shift+Enter",
                        )))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.previous_search_hit(cx);
                        })),
                )
                .child(
                    components::icon_only_button(Icon::ChevronDown, metrics)
                        .id("terminal.search.next")
                        .debug_selector(|| "terminal.search.next".to_owned())
                        .tooltip(components::tooltip(with_key(
                            strings.terminal_search_next,
                            "Enter",
                        )))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.next_search_hit(cx);
                        })),
                ),
        );
        bar.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .child(Self::search_switch(
                    "Aa",
                    with_key(strings.terminal_search_case, "Alt+C"),
                    self.search_options.case_sensitive,
                    metrics,
                    "terminal.search.case",
                    Self::toggle_case_sensitive,
                    cx,
                ))
                .child(Self::search_switch(
                    ".*",
                    with_key(strings.terminal_search_regex, "Alt+R"),
                    self.search_options.regex,
                    metrics,
                    "terminal.search.regex",
                    Self::toggle_regex,
                    cx,
                )),
        )
        .child(hint)
        .child(
            components::icon_only_button(Icon::Close, metrics)
                .id("terminal.search.close")
                .debug_selector(|| "terminal.search.close".to_owned())
                .tooltip(components::tooltip(with_key(
                    strings.terminal_search_close,
                    "Esc",
                )))
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.close_search(window, cx);
                })),
        )
    }

    /// One switch of the find bar.
    ///
    /// A square the size of its neighbours holding a short glyph (`Aa`, `.*`), as in an
    /// editor's find widget. Off it is quiet; on it sits on the accent tint with a thin
    /// accent edge, so the state is read from the control itself and not from colour
    /// alone. The full name and the key are in the tooltip (R3, R13). The identifier is
    /// also the debug selector, so a test clicks the same control a person does.
    fn search_switch(
        glyph: &'static str,
        tooltip: SharedString,
        on: bool,
        metrics: Metrics,
        id: &'static str,
        toggle: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let on_bg = theme::swatch(shell_palette::ACCENT_TINT);
        let on_hover = theme::swatch_over(shell_palette::SURFACE_HOVER, shell_palette::ACCENT_TINT);
        let on_border = theme::swatch(shell_palette::ACCENT);
        let on_text = theme::swatch(shell_palette::ACCENT_ON_TINT);
        let off_hover = theme::swatch_over(shell_palette::SURFACE_HOVER, shell_palette::CHROME);
        let off_text = theme::swatch(shell_palette::TEXT_SECONDARY);
        let off_hover_text = theme::swatch(shell_palette::TEXT_PRIMARY);
        let size = metrics.hit_target();
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .min_w(size)
            .h(size)
            .px(metrics.scale(spacing::NARROW))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .font_family(monospace_family())
            .text_size(metrics.caption_size())
            .cursor_pointer()
            .when(on, |pill| {
                pill.bg(on_bg)
                    .border_color(on_border)
                    .text_color(on_text)
                    .font_weight(FontWeight::MEDIUM)
                    .hover(move |style| style.bg(on_hover))
            })
            .when(!on, |pill| {
                pill.border_color(theme::clear())
                    .text_color(off_text)
                    .hover(move |style| style.bg(off_hover).text_color(off_hover_text))
            })
            .child(
                div()
                    .debug_selector(move || format!("{id}.{}", if on { "on" } else { "off" }))
                    .child(glyph),
            )
            .tooltip(components::tooltip(tooltip))
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
                    .min_w_0()
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

    /// An interactive proportional scrollbar along the right edge when history exists.
    fn scrollback_thumb(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        if self.alternate_screen() {
            return None;
        }
        let offset = self.core.display_offset();
        let history = self.core.history();
        // Drawn whenever there is history, at the bottom too: a bar that appeared only
        // after scrolling never told anyone there was anything above to scroll to.
        if history == 0 {
            return None;
        }
        let rows = self.grid.rows.max(1);
        let total = history + rows;
        let fraction = (rows as f32 / total as f32).clamp(0.08, 1.0);
        let progress = 1.0 - (offset as f32 / history as f32).clamp(0.0, 1.0);
        let top_pct = progress * (1.0 - fraction);
        let resting_ink = theme::faded(theme::swatch(shell_palette::TEXT_MUTED), 0.45);
        let active_ink = theme::faded(theme::swatch(shell_palette::TEXT_SECONDARY), 0.78);
        let dragging = self.scrollbar_drag;
        Some(
            div()
                .id("terminal.scrollbar.track")
                .group("terminal-scrollbar")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(0.0))
                .w(px(SCROLL_TRACK_WIDTH))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        this.scrollbar_drag = true;
                        this.scrub_scrollbar(event.position.y, cx);
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(SCROLL_THUMB_INSET))
                        .top(gpui::relative(top_pct))
                        .h(gpui::relative(fraction))
                        .w(px(if dragging {
                            SCROLL_THUMB_HOVER_WIDTH
                        } else {
                            SCROLL_THUMB_WIDTH
                        }))
                        .rounded_full()
                        .bg(if dragging { active_ink } else { resting_ink })
                        .group_hover("terminal-scrollbar", move |style| {
                            style.w(px(SCROLL_THUMB_HOVER_WIDTH)).bg(active_ink)
                        })
                        .debug_selector(|| "terminal.scrollbar.thumb".to_owned()),
                ),
        )
    }

    /// The images on screen, drawn over the text rows they cover (R4).
    ///
    /// Each image is converted to the GPU's format once and kept by id; an image
    /// that left the screen is let go here, texture included, so scrolling a long
    /// history of images does not keep them all.
    fn image_layer(&self, snapshot: &Snapshot, window: &mut Window) -> Option<gpui::Div> {
        let mut cache = self.image_cache.borrow_mut();
        let visible: std::collections::HashSet<u64> =
            snapshot.images.iter().map(|image| image.id).collect();
        let gone: Vec<u64> = cache
            .keys()
            .filter(|id| !visible.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(image) = cache.remove(&id) {
                let _ = window.drop_image(image);
            }
        }
        #[cfg(test)]
        self.painted_images.set(snapshot.images.len());
        if snapshot.images.is_empty() {
            return None;
        }
        let cell = self.cell.sanitized();
        let placed: Vec<(Bounds<Pixels>, std::sync::Arc<gpui::RenderImage>)> = snapshot
            .images
            .iter()
            .map(|visible| {
                let image = cache
                    .entry(visible.id)
                    .or_insert_with(|| std::sync::Arc::new(render_image(&visible.image)))
                    .clone();
                let bounds = Bounds::new(
                    point(
                        px(visible.column as f32 * cell.width),
                        px(visible.row as f32 * cell.height),
                    ),
                    gpui::size(
                        px(visible.image.width as f32),
                        px(visible.image.height as f32),
                    ),
                );
                (bounds, image)
            })
            .collect();
        // Laid over the rows, from the grid's own top left corner: in the rows'
        // flow it would start below the last row and every image would land a whole
        // grid lower than its cells.
        Some(
            div()
                .absolute()
                .inset_0()
                .debug_selector(|| "terminal.images".to_owned())
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            for (at, image) in placed {
                                let target = Bounds::new(bounds.origin + at.origin, at.size);
                                let _ = window.paint_image(
                                    target,
                                    gpui::Corners::default(),
                                    image,
                                    0,
                                    false,
                                );
                            }
                        },
                    )
                    .absolute()
                    .inset_0(),
                ),
        )
    }

    /// Where the last frame drew each image, relative to the grid's top left
    /// corner, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn image_origins_for_test(&self) -> Vec<gpui::Point<Pixels>> {
        let cell = self.cell.sanitized();
        self.core
            .snapshot()
            .images
            .iter()
            .map(|image| {
                point(
                    px(image.column as f32 * cell.width),
                    px(image.row as f32 * cell.height),
                )
            })
            .collect()
    }

    /// Scrolls the history by `lines` (positive is back), for tests.
    #[cfg(test)]
    pub fn scroll_for_test(&mut self, lines: i32, cx: &mut Context<Self>) {
        self.core.scroll(lines);
        cx.notify();
    }

    /// How many images the last frame drew, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn painted_images_for_test(&self) -> usize {
        self.painted_images.get()
    }

    /// How many converted images the pane keeps, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn cached_images_for_test(&self) -> usize {
        self.image_cache.borrow().len()
    }

    /// A small badge in the terminal's bar while its output is being logged, with the
    /// file in its tooltip (R3). It sits with the bar's tools, never over the grid.
    fn log_badge(&self, metrics: Metrics) -> Option<gpui::Stateful<gpui::Div>> {
        let (path, _format) = self.logging()?;
        let ux = crate::app::copy::ux_for(self.locale);
        let path = path.display().to_string();
        Some(
            div()
                .id("terminal.log.indicator")
                .debug_selector(|| "terminal.log.indicator".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::HALF))
                .px(metrics.scale(spacing::SNUG))
                .py(metrics.scale(spacing::HAIRLINE + 1.0))
                .rounded(px(theme::radius::LARGE))
                .bg(theme::swatch(shell_palette::RAISED))
                .border_1()
                .border_color(theme::swatch(shell_palette::BORDER_STRONG))
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                .child(
                    div()
                        .size(metrics.scale(spacing::NARROW))
                        .rounded_full()
                        .bg(theme::swatch(shell_palette::DANGER)),
                )
                .child(ux.terminal_log_indicator)
                .tooltip(components::tooltip(SharedString::from(path))),
        )
    }

    /// The target of the OSC 8 link under the pointer, at the bottom left of the grid,
    /// so a person can read where a link goes before opening it (R1).
    fn link_target_hint(&self, metrics: Metrics) -> Option<gpui::Stateful<gpui::Div>> {
        let link = self.hovered_link.as_ref()?;
        if link.kind != LinkKind::Osc8 {
            return None;
        }
        Some(
            div()
                .id("terminal.link_target")
                .debug_selector(|| "terminal.link_target".to_owned())
                .absolute()
                .left(metrics.scale(spacing::SNUG))
                .bottom(metrics.scale(spacing::SNUG))
                .max_w(gpui::relative(0.8))
                .overflow_hidden()
                .px(metrics.scale(spacing::SNUG))
                .py(metrics.scale(spacing::HALF))
                .rounded(px(theme::radius::LARGE))
                .bg(theme::swatch(shell_palette::RAISED))
                .border_1()
                .border_color(theme::swatch(shell_palette::BORDER_STRONG))
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                .whitespace_nowrap()
                .text_ellipsis()
                .child(link.url.clone()),
        )
    }

    /// A floating pill that shows how many lines above the live prompt the viewport is
    /// and returns to the bottom in one click.
    fn scroll_bottom_pill(
        &self,
        metrics: Metrics,
        show_blocks: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if self.alternate_screen() {
            return None;
        }
        let offset = self.core.display_offset();
        if offset == 0 {
            return None;
        }
        let ux = crate::app::copy::ux_for(self.locale);
        let template = if self.unseen_output {
            ux.terminal_jump_bottom_new
        } else {
            ux.terminal_jump_bottom
        };
        let label = crate::app::copy::fill(template, &[("lines", &offset.to_string())]);
        let hover = theme::swatch(shell_palette::SURFACE_HOVER);
        let accent = theme::swatch(shell_palette::ACCENT);
        let pill_at_top = show_blocks && !self.search_dodges_bottom();
        let bottom_offset = if self.focus_mode.get() {
            metrics.hit_target() + metrics.scale(spacing::BASE)
        } else {
            metrics.scale(spacing::SNUG)
        };
        let pill = div()
            .id("terminal.scroll_bottom")
            .debug_selector(|| "terminal.scroll_bottom".to_owned())
            .absolute()
            .right(metrics.scale(spacing::SNUG))
            .when(pill_at_top, |pill| pill.top(metrics.scale(spacing::SNUG)))
            .when(!pill_at_top, |pill| pill.bottom(bottom_offset))
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .min_h(metrics.hit_target())
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::HALF))
            .rounded(px(theme::radius::LARGE))
            .bg(theme::swatch(shell_palette::RAISED))
            .border_1()
            .border_color(if self.unseen_output {
                accent
            } else {
                theme::swatch(shell_palette::BORDER_STRONG)
            })
            .shadow(theme::Elevation::Overlay.shadows())
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.reveal_input(cx);
            }))
            .when(self.unseen_output, |pill| {
                pill.child(
                    div()
                        .size(metrics.scale(spacing::NARROW))
                        .rounded_full()
                        .bg(accent)
                        .debug_selector(|| "terminal.scroll_bottom.new_output".to_owned()),
                )
            })
            .child(crate::app::icons::icon(
                Icon::ChevronDown,
                metrics.small_icon_size(),
                accent,
            ))
            .child(SharedString::from(label));
        Some(pill)
    }

    fn viewport(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let snapshot = self.core.snapshot();
        let focused = self.focus.is_focused(window);
        let weak = cx.entity().downgrade();
        let entity = cx.entity();
        let focus = self.focus.clone();
        let capture = weak.clone();
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
        if let Some(images) = self.image_layer(&snapshot, window) {
            rows = rows.child(images);
        }
        if let Some(overlay) = self.composition_overlay(&snapshot) {
            rows = rows.child(overlay);
        } else if focused
            && let Some(cursor) = snapshot.cursor
            && let Some(suffix) = self.history_completion()
        {
            let cell = self.cell.sanitized();
            rows = rows.child(
                div()
                    .absolute()
                    .left(px(cursor.column as f32 * cell.width))
                    .top(px(cursor.row as f32 * cell.height))
                    .right_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .line_height(px(cell.height))
                    .text_size(px(self.font_size))
                    .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                    .debug_selector(|| "terminal.history-completion".to_owned())
                    .child(SharedString::from(format!("{suffix}  →"))),
            );
        }

        let link_hovered = self.hovered_link.is_some() && self.link_armed;
        div()
            .on_children_prepainted(listener)
            .id("terminal.viewport.surface")
            .flex_1()
            .relative()
            .min_h_0()
            .overflow_hidden()
            .bg(color_of(palette::BACKGROUND))
            .when(link_hovered, |el| el.cursor_pointer())
            .when(!link_hovered, |el| el.cursor_text())
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !*hovered {
                    this.last_hover_position = None;
                    if this.hovered_link.take().is_some() {
                        cx.notify();
                    }
                }
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &ModifiersChangedEvent, _window, cx| {
                    this.on_modifiers_changed(event, cx);
                },
            ))
            // The grid declares its own key context so the application's shortcuts
            // can step aside: inside a terminal Escape belongs to the program.
            .key_context(crate::app::TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus)
            .child(rows)
            .child(div().absolute().inset_0().child(canvas(
                move |bounds, _window, _cx| bounds,
                move |bounds, _prepaint, window, cx| {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, entity.clone()),
                        cx,
                    );
                    // A selection drag follows the pointer and hears the release
                    // anywhere in the window, not only over the grid: the grid's own
                    // handlers only fire while it is hovered. The listeners are part
                    // of every frame, so the first motion after the press is heard
                    // even before the frame the press caused, and they do nothing
                    // unless this pane holds a drag.
                    let moved = capture.clone();
                    window.on_mouse_event(
                        move |event: &MouseMoveEvent, phase, _window, cx: &mut App| {
                            if phase == DispatchPhase::Bubble {
                                let _ = moved.update(cx, |pane, cx| pane.drag_moved(event, cx));
                            }
                        },
                    );
                    let released = capture.clone();
                    window.on_mouse_event(
                        move |event: &MouseUpEvent, phase, _window, cx: &mut App| {
                            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                                let _ =
                                    released.update(cx, |pane, cx| pane.drag_released(event, cx));
                            }
                        },
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
                    // A selection drag hears its release at the window.
                    this.release_reported(TerminalMouseButton::Left, event, cx);
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
            // Copy and paste use the everyday Ctrl+C / Ctrl+V (⌘C / ⌘V). Copy only
            // takes the key while something is selected, and then drops the
            // highlight, so the next Ctrl+C is the program's interrupt again, as in
            // other terminals. With nothing to copy, the key falls through to the
            // program untouched.
            .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                if this.copy_selection_from_key(cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &TerminalPaste, window, cx| {
                // An empty clipboard has nothing to paste: the key is the program's
                // (Ctrl+V is "insert the next key literally" in a shell).
                if cx.read_from_clipboard().is_none() {
                    cx.propagate();
                    return;
                }
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

/// A decoded image in the GPU's pixel order (BGRA), for drawing (R4).
fn render_image(image: &shellune_terminal::sixel::SixelImage) -> gpui::RenderImage {
    let mut bgra = image.rgba.clone();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    gpui::RenderImage::from_bgra(
        gpui::size(
            gpui::DevicePixels(i32::try_from(image.width).unwrap_or(i32::MAX)),
            gpui::DevicePixels(i32::try_from(image.height).unwrap_or(i32::MAX)),
        ),
        std::sync::Arc::from(bgra),
    )
}

/// The name a new log is offered under (R3): `<label>-YYYYMMDD-HHMMSSZ.log`.
///
/// The label keeps letters, digits, `-`, `_` and `.`; anything else (a path
/// separator, a colon, a space) becomes `_`, so a host such as `root@db:2222` cannot
/// name a directory or a drive. The time is UTC, marked with `Z`.
#[must_use]
pub fn default_log_name(label: &str, now_unix: u64) -> String {
    let mut source: String = label
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = source.trim_matches(|ch| ch == '.' || ch == '_').to_owned();
    source = if trimmed.is_empty() {
        "session".to_owned()
    } else {
        trimmed.chars().take(64).collect()
    };
    // "2026-11-27 03:15 UTC" -> "202611270315", then the seconds.
    let minutes: String = crate::app::hosts::format_unix(now_unix)
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let (date, time) = minutes.split_at(minutes.len().min(8));
    format!("{source}-{date}-{time}{:02}Z.log", now_unix % 60)
}

/// A terminal core that answers colour queries with the palette this window draws.
fn new_core(grid: GridSize, scrollback: usize) -> TerminalCore {
    let mut core = TerminalCore::new(grid.cols, grid.rows, scrollback);
    core.set_shown_colors(shown_color);
    // Every command of at least the shortest threshold a user can choose is reported
    // with its text; the window compares it with the threshold actually chosen (R2).
    core.set_long_command_threshold(Some(Duration::from_secs(u64::from(
        crate::app::workspace::MIN_LONG_COMMAND_SECS,
    ))));
    core
}

/// A default palette colour as the terminal draws it now (dark, or its light mapping).
///
/// Read when a program asks (OSC 10/11/4): TUIs such as editors and coding agents pick
/// their light or dark theme from the answer, and without one they assume a dark
/// background and paint dark panels over a light terminal.
fn shown_color(color: Rgb) -> Rgb {
    let value = theme::terminal_color(
        u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b),
    );
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

fn color_of(color: Rgb) -> gpui::Rgba {
    let value = u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b);
    gpui::rgb(theme::terminal_color(value))
}

/// The background a cell paints over the canvas, or `None` when it shows the canvas.
fn cell_background(cell: &RenderedCell, canvas: gpui::Rgba) -> Option<gpui::Rgba> {
    if cell.selected {
        // The selection is a token derived from the accent, resolved in the terminal's
        // own palette, which may differ from the window's.
        return Some(gpui::rgb(
            theme::palette::SELECTION.value(theme::terminal_appearance()),
        ));
    }
    let background = color_of(cell.background);
    (background != canvas).then_some(background)
}

/// The least APCA lightness contrast (|Lc|) text keeps against its background, and
/// dimmed text.
///
/// Lc 45 is APCA's floor for text that must still be read (it sits about where WCAG's
/// 3:1 does for mid-tones, but is fair to dark backgrounds, where WCAG overstates
/// contrast); Lc 30 is its floor for any text at all, which dimmed text is allowed to
/// sink to. The palettes themselves are tuned well above these, so the floor only
/// moves colours a program picked for the other kind of terminal.
const TEXT_CONTRAST_FLOOR: f32 = 45.0;
const DIM_CONTRAST_FLOOR: f32 = 30.0;

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
    if apca(foreground, background).abs() >= floor {
        return foreground;
    }
    // Towards black on a light background, towards white on a dark one: whichever
    // end can reach the floor at all, preferring the one away from the background.
    let black = gpui::Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    let white = gpui::Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    let target = if apca(black, background).abs() >= apca(white, background).abs() {
        black
    } else {
        white
    };
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..12 {
        let middle = (low + high) / 2.0;
        if apca(mix(foreground, target, middle), background).abs() >= floor {
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

/// APCA screen luminance (SAPC 0.0.98G), with the soft clamp for near-black.
fn apca_luminance(color: gpui::Rgba) -> f32 {
    let y = 0.212_672_9 * color.r.max(0.0).powf(2.4)
        + 0.715_152_2 * color.g.max(0.0).powf(2.4)
        + 0.072_175 * color.b.max(0.0).powf(2.4);
    if y > 0.022 {
        y
    } else {
        y + (0.022 - y).powf(1.414)
    }
}

/// APCA lightness contrast of `text` on `background`, about -108 to 106.
///
/// Positive is dark text on a light background, negative light text on a dark one.
fn apca(text: gpui::Rgba, background: gpui::Rgba) -> f32 {
    let (text, background) = (apca_luminance(text), apca_luminance(background));
    if (background - text).abs() < 0.0005 {
        return 0.0;
    }
    if background > text {
        let s = (background.powf(0.56) - text.powf(0.57)) * 1.14;
        if s < 0.1 { 0.0 } else { (s - 0.027) * 100.0 }
    } else {
        let s = (background.powf(0.65) - text.powf(0.62)) * 1.14;
        if s > -0.1 { 0.0 } else { (s + 0.027) * 100.0 }
    }
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
    glyphs: RunGlyphs,
    style: TextStyle,
    start: usize,
    end: usize,
}

/// ASCII shares the grid font's advance. Fallback faces and combining marks
/// are shaped per cell, then centered in the columns the terminal assigned.
enum RunGlyphs {
    Ascii(String),
    Centered(Vec<(SharedString, usize)>),
}

impl RunGlyphs {
    fn is_ascii(&self) -> bool {
        matches!(self, Self::Ascii(_))
    }

    fn push(&mut self, text: &str, columns: usize) {
        match self {
            Self::Ascii(run) => run.push_str(text),
            Self::Centered(cells) => cells.push((SharedString::from(text.to_owned()), columns)),
        }
    }
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
    if style.italic {
        piece = piece.italic();
    }
    if style.underline {
        piece = piece.underline();
    }
    if style.strike {
        piece = piece.line_through();
    }
    pieces.push(match run.glyphs {
        RunGlyphs::Ascii(text) => piece.child(SharedString::from(text)),
        RunGlyphs::Centered(cells) => {
            let layer = canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    let text_style = window.text_style();
                    // Match the grid and the old flex layout: snap each cell's origin
                    // and its centered text offset separately to physical pixels.
                    let scale = window.scale_factor();
                    let snap = |value: Pixels| (value * scale).round() / scale;
                    let row_left = bounds.left() - snap(px(start as f32 * cell.width));
                    let mut column = start;
                    for (text, columns) in cells {
                        let width = px(columns as f32 * cell.width);
                        let run = text_style.to_run(text.len());
                        let line =
                            window
                                .text_system()
                                .shape_line(text, px(font_size), &[run], None);
                        let left = row_left + snap(px(column as f32 * cell.width));
                        let offset = snap((width - line.width.ceil()) / 2.0);
                        let origin = point(left + offset, bounds.top());
                        let _ = line.paint(origin, px(cell.height), window, cx);
                        column += columns;
                    }
                },
            )
            .size_full();
            piece
                .w(px((run.end - run.start) as f32 * cell.width))
                .child(layer)
        }
    });
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
        // The frame around the grid and the bar under it follow the window's text size:
        // a bar that has no room for every tool at that size folds them into its menu
        // instead of being pushed past the pane (R3). The find bar, the block overlay and
        // the serial strip keep the regular size; they have no narrow layout of their
        // own, and at 200% they would be pushed past a 720x480 window (R4).
        let metrics = theme::metrics();
        let tools = Metrics::default();
        let bar_height = metrics.hit_target().max(metrics.scale(SHELL_BAR_HEIGHT));
        // The advance width comes from the font the window actually resolved, so
        // the grid matches what is drawn even when the platform substitutes a face.
        let _ = self.measure_cell(window);
        if self.hovered_link.is_some() {
            self.refresh_hovered_link();
        }
        let focused = self.focus.is_focused(window);
        self.drive_cursor_blink(focused, cx);

        let focus_mode = self.focus_mode.get();
        if focus_mode {
            *self.session_slot.borrow_mut() = None;
            self.bar_has_slot.set(false);
        }
        let show_blocks =
            !self.alternate_screen() && (self.search_open || self.core.shows_command_blocks());
        // The pane's own chrome - the status line, the find bar, the menu - is drawn in
        // the terminal's palette, so a dark terminal in a light window is dark to its
        // edges. The window's palette comes back after. The paste confirmation is not
        // here any more: it is a window-level dialog, so the terminal behind it cannot
        // be typed into while an answer is pending (R17).
        let window_appearance = theme::appearance();
        theme::set_appearance(theme::terminal_appearance());
        let banner = self.status_banner();
        let show_serial_strip = !focus_mode
            || !self.state.accepts_input()
            || self
                .serial
                .as_ref()
                .is_some_and(|serial| serial.note().is_some());
        let pane = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .relative()
            // Why the session ended stands above the grid it ended on, not over it: the
            // grid moves down and keeps its last screen (ADR-6).
            .children(banner)
            .child(
                // A margin inside the frame keeps the first column off the border; the
                // grid itself is measured inside it, so no cell is lost to the margin.
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .debug_selector(move || {
                        if focus_mode {
                            "terminal.chrome.focus".to_owned()
                        } else {
                            "terminal.chrome.regular".to_owned()
                        }
                    })
                    .pl(metrics.scale(spacing::SNUG))
                    .pr(metrics.scale(spacing::HAIRLINE))
                    .py(metrics.scale(spacing::TIGHT))
                    .bg(color_of(palette::BACKGROUND))
                    .child(self.viewport(window, cx))
                    .children(self.scrollback_thumb(cx))
                    .children(self.scroll_bottom_pill(metrics, show_blocks, cx))
                    .children(self.link_target_hint(metrics)),
            )
            // A serial line states itself above the bar: its device, its settings and
            // the two send habits are what a person changes while the session runs, and
            // a port that would not open says why here instead of drawing a terminal
            // that is not connected (R21).
            .when(show_serial_strip, |pane| {
                pane.when_some(self.serial_strip(tools, cx), |pane, strip| {
                    pane.child(strip.when(focus_mode, |strip| {
                        strip.pr(metrics.hit_target() + metrics.scale(spacing::LOOSE))
                    }))
                })
            })
            .when(!focus_mode, |pane| pane.child(self.bar(metrics, cx)))
            .when(show_blocks, |column| {
                column.child(self.block_overlay(metrics, bar_height, cx))
            })
            .children(self.context_menu(window, cx))
            .when(!focus_mode, |pane| {
                pane.children(self.more_menu(window, cx))
            });
        theme::set_appearance(window_appearance);
        pane
    }
}

/// One line about the serial port list, in the words the situation needs (R21).
///
/// Three situations are kept apart on purpose: a list that could not be read, an
/// answer with no ports in it, and a list that simply does not hold the saved device.
/// They have different next steps, and drawing them as one sentence would send a user
/// to plug in a cable when the device service is what needs looking at - or would make
/// a device the record names look attached because something else is.
pub(crate) fn ports_line(serial: &SerialPane, locale: Locale) -> String {
    let strings = locale.strings();
    let ports = serial.ports();
    match ports {
        // Nothing is claimed before the system has been asked.
        PortScan::NotAsked => String::new(),
        PortScan::Failed(reason) => {
            format!("{}: {reason}", strings.serial_ports_failed)
        }
        PortScan::Ready(_) if ports.is_empty_machine() => {
            format!(
                "{} - {}",
                strings.serial_ports_empty, strings.serial_ports_hint
            )
        }
        PortScan::Ready(_) => {
            let shown = ports.labels().join(", ");
            let mut line = format!("{}: {shown}", strings.serial_ports_attached);
            // The saved device is matched by its exact name, never by resemblance: a
            // COM3 that is gone is not satisfied by a COM30 that is present.
            if crate::app::serial::DeviceChoice::of(ports, serial.device())
                == crate::app::serial::DeviceChoice::Missing
            {
                line.push_str(" - ");
                line.push_str(strings.serial_ports_missing);
                line.push_str(", ");
                line.push_str(strings.serial_ports_hint);
            }
            line
        }
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
        if self.search_captures_keyboard() {
            self.append_search_query(text, cx);
            return;
        }
        // A commit is program input, including one command that ends in a newline.
        // Multi-line and high-risk confirmation belongs to clipboard paste.
        //
        // It is also what a person typed, which is why it goes through the user send
        // path and not straight into the source: on a serial line the chosen ending is
        // written for a break the commit carries, and the local echo draws the text
        // (R21). Without this a device that does not echo would leave a typist blind,
        // because every printable character and every space arrives here - the key path
        // only sees the characters that are not plain text.
        let bytes = encode_text(text);
        self.write_input(&bytes, cx);
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
        if !new_text.is_empty() && !self.search_captures_keyboard() && self.state.accepts_input() {
            self.reveal_input(cx);
        }
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
        let cursor = self.core.visible_cursor()?;
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
mod output_repaint_tests {
    use super::*;

    #[gpui::test]
    fn small_replies_repaint_immediately_and_a_burst_flushes_after_its_last_read(
        cx: &mut gpui::TestAppContext,
    ) {
        let pane = cx.new(|cx| {
            TerminalPane::without_process(
                LocalShell::new("Test shell", "test-shell"),
                EventTag::new(shellune_core::SessionId::new(), 0),
                GridSize::new(80, 24),
                shellune_terminal::DEFAULT_FONT_SIZE,
                Locale::English,
                cx,
            )
        });
        let paints = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = paints.clone();
        let _observe = cx.update(|app| app.observe(&pane, move |_, _| count.set(count.get() + 1)));
        for bytes in [1, 2, 65_536, 1, 65_536] {
            pane.update(cx, |pane, cx| pane.repaint_ssh_output(bytes, cx));
        }
        cx.run_until_parked();
        assert_eq!(paints.get(), 2);
        cx.executor().advance_clock(Duration::from_millis(32));
        cx.run_until_parked();
        assert_eq!(paints.get(), 3);
        cx.executor().advance_clock(Duration::from_millis(100));
        pane.update(cx, |pane, cx| pane.repaint_ssh_output(1, cx));
        cx.run_until_parked();
        assert_eq!(paints.get(), 4);
    }
}

#[cfg(test)]
mod ink_tests {
    use super::{apca, ink};

    fn contrast(text: gpui::Rgba, background: gpui::Rgba) -> f32 {
        apca(text, background).abs()
    }

    fn rgb(hex: u32) -> gpui::Rgba {
        gpui::rgb(hex)
    }

    #[test]
    fn a_pale_colour_on_a_light_canvas_is_made_legible() {
        // A grey chosen for a dark terminal, on the light one.
        let drawn = ink(rgb(0xe6e6e6), rgb(0xfcfcfd), false);
        assert!(contrast(drawn, rgb(0xfcfcfd)) >= 44.9);
    }

    #[test]
    fn a_dark_colour_on_a_dark_canvas_is_made_legible() {
        let drawn = ink(rgb(0x1a1c22), rgb(0x0b0c10), false);
        assert!(contrast(drawn, rgb(0x0b0c10)) >= 44.9);
    }

    #[test]
    fn white_text_on_a_black_cell_stays_readable_in_the_light_terminal() {
        // The pair a status line or a prompt segment paints: the light terminal keeps
        // black dark and white light, so the pair reads without any correction.
        let black = rgb(crate::app::theme::terminal_light_for_test(0x353941));
        let white = rgb(crate::app::theme::terminal_light_for_test(0xbabec4));
        assert!(contrast(white, black) >= 60.0);
        assert_eq!(ink(white, black, false), white);
    }

    #[test]
    fn apca_matches_the_reference_values() {
        // Reference pairs from the APCA documentation.
        assert!((apca(rgb(0x888888), rgb(0xffffff)) - 63.1).abs() < 0.2);
        assert!((apca(rgb(0xffffff), rgb(0x888888)) + 68.5).abs() < 0.2);
        assert!((apca(rgb(0x000000), rgb(0xaaaaaa)) - 58.1).abs() < 0.2);
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
        assert!(contrast(dim, canvas) >= 29.9);
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

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[gpui::test]
    async fn history_completion_accepts_edits_and_preserves_tab(cx: &mut gpui::TestAppContext) {
        let (pane, cx) = cx.add_window_view(|_, cx| {
            TerminalPane::without_process(
                LocalShell::new("Test shell", "test-shell"),
                EventTag::new(shellune_core::SessionId::new(), 0),
                GridSize::new(80, 24),
                shellune_terminal::DEFAULT_FONT_SIZE,
                Locale::English,
                cx,
            )
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            window.focus(pane.read(app).focus_handle());
            pane.update(app, |pane, cx| {
                pane.feed_for_test(b"\x1b]133;B\x07echo hello\x1b]133;C\x07\r\nhello\r\n\x1b]133;D;0\x07$ \x1b]133;B\x07ec", cx);
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("terminal.history-completion").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("terminal.history-completion").is_none());
        cx.update(|_, app| pane.update(app, |pane, cx| pane.feed_for_test(b"h", cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("right");
        cx.simulate_input("!");
        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            b"o hello!"
        );
        cx.simulate_keystrokes("tab enter");
        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            b"o hello!\t\r"
        );
    }
}

#[cfg(test)]
mod flood_batch_tests {
    use super::*;
    #[test]
    fn adjacent_output_is_coalesced_without_moving_exit_or_status() {
        let events = coalesce_output(vec![
            SourceEvent::Data(vec![1]),
            SourceEvent::Data(vec![2]),
            SourceEvent::Exited(ExitReport::Clean),
            SourceEvent::Data(vec![3]),
        ]);
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], SourceEvent::Data(bytes) if bytes == &[1, 2]));
        assert!(matches!(&events[1], SourceEvent::Exited(ExitReport::Clean)));
        assert!(matches!(&events[2], SourceEvent::Data(bytes) if bytes == &[3]));
    }
}

#[cfg(test)]
mod selection_drag_tests {
    use super::*;

    #[gpui::test]
    async fn slow_drag_during_streaming_output_keeps_original_anchor_line(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = cx.add_window_view(|_, cx| {
            TerminalPane::without_process(
                LocalShell::new("Test shell", "test-shell"),
                EventTag::new(shellune_core::SessionId::new(), 0),
                GridSize::new(80, 24),
                shellune_terminal::DEFAULT_FONT_SIZE,
                Locale::English,
                cx,
            )
        });
        cx.run_until_parked();

        // Fill the screen with numbered lines so row 5 holds a distinct marker.
        let rows = cx.update(|_, app| pane.read(app).grid.rows);
        assert!(rows >= 8, "test viewport should have at least 8 rows");
        let target_row = 5usize;
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                for index in 0..rows {
                    let line = if index + 1 < rows {
                        format!("line-{index:02}-anchor-test\r\n")
                    } else {
                        format!("line-{index:02}-anchor-test")
                    };
                    pane.feed_for_test(line.as_bytes(), cx);
                }
            });
        });
        cx.run_until_parked();

        let (down_pos, drag_pos) = cx.update(|_, app| {
            let pane = pane.read(app);
            let bounds = pane.viewport.expect("viewport measured");
            let cell = pane.cell.sanitized();
            let y = f32::from(bounds.origin.y) + (target_row as f32 + 0.5) * cell.height;
            let x = f32::from(bounds.origin.x) + 0.25 * cell.width;
            (
                point(px(x), px(y)),
                point(px(x + SELECTION_DRAG_THRESHOLD + cell.width * 6.0), px(y)),
            )
        });

        // Mouse down records the anchor without starting the selection yet.
        cx.simulate_mouse_down(down_pos, MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(
            cx.update(|_, app| pane.read(app).drag.is_some_and(|drag| !drag.started)),
            "a single press waits for the drag threshold before starting selection"
        );

        // Streaming output arrives before the pointer crosses the threshold,
        // scrolling the target line up by 4 rows.
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                for index in 0..4 {
                    pane.feed_for_test(format!("\r\nstream-{index:02}-extra").as_bytes(), cx);
                }
            });
        });
        cx.run_until_parked();

        // Move past the threshold and release.
        cx.simulate_mouse_move(
            drag_pos,
            Some(MouseButton::Left),
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_up(drag_pos, MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();

        let selected = cx.update(|_, app| pane.read(app).core.selection_text());
        let selected = selected.expect("selection should be established after drag");
        assert!(
            selected.starts_with(&format!("line-{target_row:02}")),
            "selection start must stay anchored to line-{target_row:02} despite streaming scroll, got: {selected:?}"
        );
    }
}

#[cfg(test)]
mod search_isolation_tests {
    use super::*;

    fn test_pane(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<TerminalPane>, &mut gpui::VisualTestContext) {
        let (pane, cx) = cx.add_window_view(|_, cx| {
            TerminalPane::without_process(
                LocalShell::new("Test shell", "test-shell"),
                EventTag::new(shellune_core::SessionId::new(), 0),
                GridSize::new(80, 24),
                shellune_terminal::DEFAULT_FONT_SIZE,
                Locale::English,
                cx,
            )
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            window.focus(pane.read(app).focus_handle());
        });
        cx.run_until_parked();
        (pane, cx)
    }

    fn feed_scrollback(pane: &gpui::Entity<TerminalPane>, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                let mut text = String::new();
                for index in 0..80 {
                    text.push_str(&format!("history-line-{index:03}\r\n"));
                }
                pane.feed_for_test(text.as_bytes(), cx);
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn search_focused_handles_edit_and_navigation_keys_without_shell_leak(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = test_pane(cx);
        feed_scrollback(&pane, cx);

        // Park viewport in scrollback history.
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                pane.core.scroll(10);
                cx.notify();
            });
        });
        cx.run_until_parked();
        let parked = cx.update(|_, app| pane.read(app).display_offset());
        assert!(parked > 0, "viewport must start parked in scrollback");

        // Open search and type a query.
        cx.update(|window, app| {
            pane.update(app, |pane, cx| pane.open_search(window, cx));
        });
        cx.run_until_parked();
        cx.simulate_input("abcd");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "abcd"
        );

        // Navigation and editing inside query: Left, Left, insert 'X', Delete, Home, Delete, End, Backspace.
        cx.simulate_keystrokes("left left");
        cx.simulate_input("X");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "abXcd"
        );

        cx.simulate_keystrokes("delete");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "abXd"
        );

        cx.simulate_keystrokes("home delete");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "bXd"
        );

        cx.simulate_keystrokes("end backspace");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "bX"
        );

        // Ctrl editing combinations and Tab/Up/Down/Ctrl+C/Ctrl+L must never leak to shell.
        cx.simulate_input(" foo bar");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-backspace");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "bX foo "
        );

        // Home, Ctrl+Right word jump, Ctrl+K, and Ctrl+A select-all replacement.
        cx.simulate_keystrokes("home ctrl-right");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_cursor_for_test()),
            2
        );
        cx.simulate_keystrokes("home right ctrl-k");
        cx.run_until_parked();
        assert_eq!(cx.update(|_, app| pane.read(app).search_state().query), "b");

        cx.simulate_input(" zzq zzw");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-a");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_selection_for_test()),
            Some(0.."b zzq zzw".len())
        );
        cx.simulate_input("zzx");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "zzx"
        );

        cx.simulate_keystrokes("ctrl-e ctrl-u");
        cx.run_until_parked();
        assert_eq!(cx.update(|_, app| pane.read(app).search_state().query), "");

        cx.simulate_keystrokes("tab up down pageup pagedown ctrl-c ctrl-l");
        cx.run_until_parked();

        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            Vec::<u8>::new(),
            "no key pressed while search holds focus may leak to the shell"
        );
        assert_eq!(
            cx.update(|_, app| pane.read(app).display_offset()),
            parked,
            "search keys must not pull a parked viewport back to the bottom"
        );
    }

    #[gpui::test]
    async fn search_focused_blocks_image_clipboard_paste_for_local_and_remote(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = test_pane(cx);
        let image_requests = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let image_requests_sub = image_requests.clone();
        cx.update(|_, app| {
            app.subscribe(&pane, move |_, event: &PaneRequest, _| {
                if matches!(event, PaneRequest::PasteImage(_)) {
                    image_requests_sub.set(image_requests_sub.get() + 1);
                }
            })
            .detach();
        });

        let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![0x89, b'P', b'N', b'G']);
        cx.write_to_clipboard(ClipboardItem::new_image(&image));
        cx.update(|window, app| {
            pane.update(app, |pane, cx| pane.open_search(window, cx));
        });
        cx.run_until_parked();

        // Local origin: must NOT send 0x16.
        cx.dispatch_action(TerminalPaste);
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            Vec::<u8>::new(),
            "image paste while search is focused must not send 0x16 to local shell"
        );

        // Remote origin: must NOT emit PaneRequest::PasteImage.
        cx.update(|_, app| {
            pane.update(app, |pane, _| {
                pane.origin = PaneOrigin::Remote {
                    target: "host@example".to_owned(),
                };
            });
        });
        cx.dispatch_action(TerminalPaste);
        cx.run_until_parked();
        assert_eq!(
            image_requests.get(),
            0,
            "image paste while search is focused must not emit PasteImage for remote pane"
        );
    }

    #[gpui::test]
    async fn search_bar_shows_inline_ime_composition_instead_of_grid_overlay(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = test_pane(cx);
        feed_scrollback(&pane, cx);
        cx.update(|window, app| {
            pane.update(app, |pane, cx| {
                pane.core.scroll(8);
                pane.open_search(window, cx);
            });
        });
        cx.run_until_parked();
        let parked = cx.update(|_, app| pane.read(app).display_offset());

        cx.update(|window, app| {
            use gpui::EntityInputHandler;
            pane.update(app, |pane, cx| {
                pane.replace_and_mark_text_in_range(None, "nihao", None, window, cx);
            });
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("terminal.search.composition").is_some(),
            "IME pre-edit text must render inline inside the search bar"
        );
        assert!(
            cx.debug_bounds("terminal.composition").is_none(),
            "IME pre-edit text must not draw on the terminal grid cursor while search is focused"
        );
        assert_eq!(
            cx.update(|_, app| pane.read(app).display_offset()),
            parked,
            "IME composition in search must not pull parked scrollback to the bottom"
        );

        cx.update(|window, app| {
            use gpui::EntityInputHandler;
            pane.update(app, |pane, cx| {
                pane.replace_text_in_range(None, "你好", window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "你好"
        );
        assert!(cx.debug_bounds("terminal.search.composition").is_none());
        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            Vec::<u8>::new()
        );
    }

    #[gpui::test]
    async fn alternate_screen_disables_context_menu_search_and_gates_paste(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = test_pane(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("echo hi".to_owned()));

        // Enter alternate screen and open right-click menu.
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                pane.feed_for_test(b"\x1b[?1049h\x1b[Halt", cx);
                pane.menu = Some(point(px(40.0), px(40.0)));
                cx.notify();
            });
        });
        cx.run_until_parked();

        let search_center = cx
            .debug_bounds("terminal.menu.search")
            .expect("search item rendered in menu")
            .center();
        cx.simulate_click(search_center, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(
            !cx.update(|_, app| pane.read(app).search_state().open),
            "clicking disabled search item in alternate screen must not open search"
        );

        // If search was parked while entering alternate screen and the session has ended,
        // paste must be disabled instead of routing through the search branch.
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                pane.search_open = true;
                pane.search_focused = true;
                pane.state = PaneState::Exited {
                    report: ExitReport::Clean,
                };
                pane.menu = Some(point(px(40.0), px(40.0)));
                cx.notify();
            });
        });
        cx.run_until_parked();

        let paste_center = cx
            .debug_bounds("terminal.menu.paste")
            .expect("paste item rendered in menu")
            .center();
        cx.simulate_click(paste_center, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| pane.read(app).search_state().query),
            "",
            "disabled paste in alternate screen must not mutate parked search query"
        );
        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            Vec::<u8>::new(),
            "disabled paste in exited alternate screen must not write to shell"
        );
    }

    #[gpui::test]
    async fn search_switches_use_capsule_indicators_and_scale_with_theme_metrics(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = test_pane(cx);
        cx.update(|window, app| {
            theme::set_zoom(theme::Zoom::Regular);
            pane.update(app, |pane, cx| pane.open_search(window, cx));
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("terminal.search.case.off").is_some());
        assert!(cx.debug_bounds("terminal.search.regex.off").is_some());
        let regular_height = cx
            .debug_bounds("terminal.search.case")
            .expect("case switch bounds")
            .size
            .height;

        cx.simulate_keystrokes("alt-c alt-r");
        cx.run_until_parked();
        assert!(cx.debug_bounds("terminal.search.case.on").is_some());
        assert!(cx.debug_bounds("terminal.search.regex.on").is_some());

        cx.update(|_, app| {
            theme::set_zoom(theme::Zoom::Comfortable);
            pane.update(app, |_, cx| cx.notify());
        });
        cx.run_until_parked();
        let comfortable_height = cx
            .debug_bounds("terminal.search.case")
            .expect("case switch bounds at 115%")
            .size
            .height;
        assert!(
            comfortable_height > regular_height,
            "search bar controls must scale with theme::metrics(): {regular_height:?} vs {comfortable_height:?}"
        );
        theme::set_zoom(theme::Zoom::Regular);
    }

    #[gpui::test]
    async fn scroll_bottom_pill_returns_to_live_output_without_writing_to_shell(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pane, cx) = test_pane(cx);
        feed_scrollback(&pane, cx);
        cx.update(|_, app| {
            pane.update(app, |pane, cx| {
                pane.core.scroll(12);
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(cx.update(|_, app| pane.read(app).display_offset()) > 0);
        let pill = cx
            .debug_bounds("terminal.scroll_bottom")
            .expect("scroll_bottom_pill visible when scrolled back");
        cx.simulate_click(pill.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            cx.update(|_, app| pane.read(app).display_offset()),
            0,
            "clicking scroll_bottom_pill must return to live bottom"
        );
        assert_eq!(
            cx.update(|_, app| pane.read(app).sent().to_vec()),
            Vec::<u8>::new(),
            "returning to live output must not write any bytes to the shell"
        );
    }
}
