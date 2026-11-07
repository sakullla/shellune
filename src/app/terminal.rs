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
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, ScrollDelta, ScrollWheelEvent, SharedString, Size, UTF16Selection, Window, canvas, div,
    px,
};
use std::ops::Range;

use shellune_core::{EventTag, LocalShell, SessionFailure};
use shellune_ssh::{ChainEvent, ChainSession, RemoteEvent, RemoteSession};
use shellune_terminal::{
    CellSize, CursorShape, ExitReport, GridSize, Key, KeyModifiers, LocalSession, PtyEvent, Rgb,
    SelectionType, Snapshot, TerminalCore, TerminalEffects, encode_key, encode_paste, encode_text,
    grid_for, palette, sanitize_font_size, step_font_size,
};

use crate::app::components;
use crate::app::copy::Locale;
use crate::app::icons::Icon;
use crate::app::theme::{self, Metrics, Tone, palette as shell_palette, spacing};

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
    marked: Option<String>,
    last_copy: Option<String>,
    locale: Locale,
    polling: bool,
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
        let (source, state) = match LocalSession::spawn(&shell, grid, None) {
            Ok(session) => (Some(PaneSource::Local(session)), PaneState::Live),
            Err(error) => (None, PaneState::Failed(error.session_failure())),
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
            marked: None,
            last_copy: None,
            locale,
            polling: false,
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
            marked: None,
            last_copy: None,
            locale,
            polling: false,
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
            marked: None,
            last_copy: None,
            locale,
            polling: false,
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
            marked: None,
            last_copy: None,
            locale,
            polling: false,
        }
    }

    /// Feeds bytes as if the shell had produced them.
    #[cfg(test)]
    pub fn feed_for_test(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let effects = self.core.feed(bytes);
        self.apply_effects(effects, cx);
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
                let effects = self.core.feed(&bytes);
                self.apply_effects(effects, cx);
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
        }
    }

    fn apply_effects(&mut self, effects: TerminalEffects, cx: &mut Context<Self>) {
        if !effects.writes.is_empty() {
            // A device-attribute query has to be answered or the program that asked
            // waits for ever; the answer goes back through the same PTY.
            self.write(&effects.writes, cx);
        }
        if let Some(text) = effects.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
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
        let keystroke = &event.keystroke;
        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            platform: keystroke.modifiers.platform,
        };

        // The terminal's own clipboard shortcuts come first: they must not reach
        // the shell, where Ctrl+C means "interrupt".
        if modifiers.control && modifiers.shift {
            match keystroke.key.as_str() {
                "c" => {
                    self.copy_selection(cx);
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    self.paste(window, cx);
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

    /// Puts the selection on the clipboard.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.core.selection_text().filter(|text| !text.is_empty()) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.last_copy = Some(text);
        cx.notify();
    }

    /// Sends pasted text, wrapped when the running program asked for bracketed paste.
    fn paste(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bytes = encode_paste(&text, self.core.mode());
        self.write(&bytes, cx);
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
        if let Some(source) = self.source.as_mut()
            && let Err(_reason) = source.resize(grid)
        {
            // A PTY that rejects the size keeps its old size; the grid still
            // changes so the user sees what the program wrote at that width.
        }
        cx.notify();
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
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

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        let Some((row, column)) = self.cell_at(event.position) else {
            return;
        };
        self.dragging = true;
        self.press = Some((row, column));
        self.core
            .begin_selection(row, column, SelectionType::Simple);
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.dragging {
            return;
        }
        let Some((row, column)) = self.cell_at(event.position) else {
            return;
        };
        self.core.extend_selection(row, column);
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
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
        // the button went down on.
        let moved = match (press, released) {
            (Some(press), Some(released)) => press != released,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if moved {
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
        };
        self.core.scroll(lines as i32);
        cx.notify();
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
            .bg(theme::swatch(shell_palette::SURFACE))
            .border_t_1()
            .border_color(theme::swatch(shell_palette::BORDER))
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
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(1.0))
                    .child(
                        components::icon_only_button(Icon::Copy, metrics)
                            .h(metrics.scale(22.0))
                            .w(metrics.scale(24.0))
                            .id("terminal.copy")
                            .debug_selector(|| "terminal.copy".to_owned())
                            .tooltip(components::tooltip(SharedString::from(format!(
                                "{} (Ctrl+Shift+C)",
                                strings.terminal_copy
                            ))))
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
                            .h(metrics.scale(22.0))
                            .min_w(metrics.scale(24.0))
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
                            .h(metrics.scale(22.0))
                            .min_w(metrics.scale(24.0))
                            .id("terminal.font.increase")
                            .debug_selector(|| "terminal.font.increase".to_owned())
                            .tooltip(components::tooltip(strings.terminal_larger))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.step_font_size(true, cx);
                            })),
                    ),
            )
    }

    fn row(&self, index: usize, snapshot: &Snapshot) -> gpui::Div {
        let cell = self.cell.sanitized();
        let default_background = color_of(palette::BACKGROUND);
        let mut element = div()
            .flex()
            .flex_none()
            .items_start()
            .relative()
            .w_full()
            .h(px(cell.height))
            .whitespace_nowrap()
            .debug_selector(move || format!("terminal.row.{index}"));

        let Some(row) = snapshot.rows.get(index) else {
            return element;
        };

        let mut run = String::new();
        let mut style: Option<(gpui::Rgba, gpui::Rgba, bool, bool, bool)> = None;
        for rendered in &row.cells {
            let key = (
                color_of(rendered.foreground),
                color_of(rendered.background),
                rendered.attributes.bold,
                rendered.attributes.underline,
                rendered.selected,
            );
            if style != Some(key) {
                element = append_run(
                    element,
                    std::mem::take(&mut run),
                    style,
                    default_background,
                    cell,
                    self.font_size,
                );
                style = Some(key);
            }
            // A wide character already covers the following column, so only its
            // own glyph is written and the spacer stays out of the row.
            run.push_str(&rendered.text);
        }
        element = append_run(
            element,
            run,
            style,
            default_background,
            cell,
            self.font_size,
        );

        if let Some(cursor) = snapshot.cursor
            && cursor.row == index
        {
            let (width, height, top) = match cursor.shape {
                CursorShape::Beam => (CARET_WIDTH, cell.height, 0.0),
                CursorShape::Underline => (cell.width, CARET_WIDTH, cell.height - CARET_WIDTH),
                _ => (CARET_WIDTH, cell.height, 0.0),
            };
            element = element.child(
                div()
                    .absolute()
                    .left(px(cursor.column as f32 * cell.width))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .bg(theme::swatch(shell_palette::ACCENT)),
            );
        }

        element
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

    fn viewport(&self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let snapshot = self.core.snapshot();
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
            rows = rows.child(self.row(index, &snapshot));
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
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                this.on_scroll(event, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }))
    }
}

fn color_of(color: Rgb) -> gpui::Rgba {
    let value = u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b);
    gpui::rgb(theme::terminal_color(value))
}

fn append_run(
    element: gpui::Div,
    run: String,
    style: Option<(gpui::Rgba, gpui::Rgba, bool, bool, bool)>,
    default_background: gpui::Rgba,
    cell: CellSize,
    font_size: f32,
) -> gpui::Div {
    if run.is_empty() {
        return element;
    }
    let Some((foreground, background, bold, underline, selected)) = style else {
        return element;
    };
    let background = if selected {
        gpui::rgba(match theme::appearance() {
            theme::Appearance::Dark => SELECTION_BACKGROUND,
            theme::Appearance::Light => SELECTION_BACKGROUND_LIGHT,
        })
    } else {
        background
    };

    let mut span = div()
        .flex_none()
        .h_full()
        .text_size(px(font_size))
        .line_height(px(cell.height))
        .text_color(foreground)
        .font_weight(if bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        });
    if underline {
        span = span.underline();
    }
    if background != default_background {
        // The viewport already paints the default background, so a run only needs
        // its own when the program asked for a different one.
        span = span.bg(background);
    }
    element.child(span.child(SharedString::from(run)))
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        // The advance width comes from the font the window actually resolved, so
        // the grid matches what is drawn even when the platform substitutes a face.
        let _ = self.measure_cell(window);

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.0))
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
