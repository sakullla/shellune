//! Command blocks from explicit shell-integration markers (R5).
//!
//! A block exists only because a shell emitted an OSC 133 or OSC 633 sequence:
//! prompt start (`A`), input start (`B`), output start (`C`), command end (`D`),
//! or an explicit command line (`E`). Prompt text is not scanned, and a process
//! exit status is never read here. A missing or unreadable exit code stays
//! unknown; it is not success.
//!
//! Nothing in this module writes a shell startup file. Recognising markers is
//! not an installer.

use std::time::{Duration, Instant};

use crate::core::{Matcher, SearchFailure, SearchOptions};

/// Stable id of one command block inside a single terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandBlockId(u64);

impl CommandBlockId {
    /// The raw id, for labels and tests.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What is known about how a command ended.
///
/// [`BlockStatus::Exited`] with code `0` is the only success. Running and unknown
/// are never success, including when the code was absent or not an integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockStatus {
    /// No command-end marker has arrived.
    Running,
    /// The command ended, but no usable exit code arrived with the marker.
    Unknown,
    /// The command-end marker included this exit code.
    Exited(i32),
}

impl BlockStatus {
    /// Whether the shell reported a zero exit code.
    ///
    /// Unknown and still-running commands are not success.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Exited(0))
    }
}

/// Which blocks a search keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockFilter {
    /// Every block.
    #[default]
    All,
    /// Still waiting for a command-end marker.
    Running,
    /// Ended without an exit code.
    Unknown,
    /// Ended with exit code 0. This is the only success filter.
    ExitedZero,
    /// Ended with a non-zero exit code.
    Failed,
}

impl BlockFilter {
    /// The next filter, in an order a single button can cycle through.
    #[must_use]
    pub const fn cycle(self) -> Self {
        match self {
            Self::All => Self::Running,
            Self::Running => Self::Unknown,
            Self::Unknown => Self::Failed,
            Self::Failed => Self::ExitedZero,
            Self::ExitedZero => Self::All,
        }
    }

    const fn matches(self, status: BlockStatus) -> bool {
        match self {
            Self::All => true,
            Self::Running => matches!(status, BlockStatus::Running),
            Self::Unknown => matches!(status, BlockStatus::Unknown),
            Self::ExitedZero => status.is_success(),
            Self::Failed => matches!(status, BlockStatus::Exited(code) if code != 0),
        }
    }
}

/// One real command: the text the shell reported, its output, and what is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBlock {
    /// Identity of this execution. A rerun is a different id.
    pub id: CommandBlockId,
    /// Command text taken from an `E` marker or from the grid between input and output.
    pub command: String,
    /// Output taken from the grid between output start and command end.
    pub output: String,
    /// Exit knowledge. Never invented from a process status.
    pub status: BlockStatus,
    /// Time from output start to command end, when both markers arrived.
    pub duration: Option<Duration>,
}

impl CommandBlock {
    /// Command and output as one clipboard payload.
    ///
    /// Both parts were copied off the grid (or an explicit command marker), so the
    /// text is the terminal's text, not a reconstruction from a side log.
    #[must_use]
    pub fn copy_text(&self) -> String {
        match (self.command.is_empty(), self.output.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.command.clone(),
            (true, false) => self.output.clone(),
            (false, false) => format!("{}\n{}", self.command, self.output),
        }
    }
}

/// A point in the grid that stays put while lines scroll into history.
///
/// `stable` is `history_size + line + lines_dropped` at capture time. Converting
/// it back uses the history size at read time, so a line that moved into the
/// scrollback is still the same line until it falls out of the scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridAnchor {
    pub stable: i64,
    pub column: usize,
}

/// The exit field of a command-end marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitCodeField {
    /// `D` with no code.
    Missing,
    /// `D` with an integer code.
    Known(i32),
    /// `D` with a code that is not an integer. The integration can no longer be trusted.
    Unreadable,
}

/// One shell-integration event, after the OSC wrapper has been removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IntegrationEvent {
    /// `A`: the prompt, and therefore a command boundary, started.
    PromptStart,
    /// `B`: the user input started.
    InputStart,
    /// `C`: the command was submitted and output started.
    OutputStart,
    /// `D`: the command ended, with whatever exit field was present.
    CommandFinished { exit: ExitCodeField },
    /// `E`: the shell stated the command text.
    CommandText(String),
}

/// A slice of a PTY chunk: bytes the grid should parse, or a marker it should not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    /// Bytes that are not a command-block marker.
    Bytes(Vec<u8>),
    /// A complete OSC 133 / OSC 633 marker. It is not part of the grid text.
    Event(IntegrationEvent),
}

/// Splits a byte stream so command markers can be handled beside the grid parser.
///
/// An incomplete OSC at the end of a chunk is held until the terminator arrives,
/// so a marker cut between two reads is still one marker. Any other sequence,
/// including OSC 0 and OSC 52, is returned unchanged for the grid parser.
#[derive(Debug, Default)]
pub(crate) struct IntegrationSplitter {
    held: Vec<u8>,
}

impl IntegrationSplitter {
    pub(crate) fn new() -> Self {
        Self { held: Vec::new() }
    }

    /// Appends `input` and returns every complete segment it now contains.
    pub(crate) fn push(&mut self, input: &[u8]) -> Vec<Segment> {
        self.held.extend_from_slice(input);
        let mut out = Vec::new();
        let mut plain = Vec::new();
        loop {
            match take_one(&self.held) {
                Take::NeedMore => break,
                Take::Plain(0) => break,
                Take::Plain(n) => {
                    let n = n.min(self.held.len());
                    plain.extend(self.held.drain(..n));
                }
                Take::Event(n, event) => {
                    if !plain.is_empty() {
                        out.push(Segment::Bytes(std::mem::take(&mut plain)));
                    }
                    let n = n.min(self.held.len());
                    self.held.drain(..n);
                    out.push(Segment::Event(event));
                }
            }
        }
        if !plain.is_empty() {
            out.push(Segment::Bytes(plain));
        }
        out
    }
}

enum Take {
    NeedMore,
    Plain(usize),
    Event(usize, IntegrationEvent),
}

/// How long an unfinished OSC may be held before it is treated as ordinary text.
///
/// A program that emits ESC `]` and then a long run of normal output must not
/// stall the grid waiting for a terminator that is not coming.
const MAX_HELD_OSC: usize = 8192;

fn take_one(data: &[u8]) -> Take {
    if data.is_empty() {
        return Take::NeedMore;
    }
    let Some(esc) = data.iter().position(|byte| *byte == 0x1b) else {
        return Take::Plain(data.len());
    };
    if esc > 0 {
        return Take::Plain(esc);
    }
    if data.len() == 1 {
        return Take::NeedMore;
    }
    if data[1] != b']' {
        return Take::Plain(1);
    }
    let mut index = 2;
    while index < data.len() {
        if data[index] == 0x07 {
            return classify(&data[2..index], index + 1);
        }
        if data[index] == 0x1b {
            if index + 1 >= data.len() {
                return if data.len() > MAX_HELD_OSC {
                    Take::Plain(1)
                } else {
                    Take::NeedMore
                };
            }
            if data[index + 1] == b'\\' {
                return classify(&data[2..index], index + 2);
            }
        }
        index += 1;
        if index > MAX_HELD_OSC {
            return Take::Plain(1);
        }
    }
    if data.len() > MAX_HELD_OSC {
        Take::Plain(1)
    } else {
        Take::NeedMore
    }
}

fn classify(body: &[u8], end: usize) -> Take {
    let text = String::from_utf8_lossy(body);
    let Some((code, rest)) = text.split_once(';') else {
        return Take::Plain(end);
    };
    if code != "133" && code != "633" {
        return Take::Plain(end);
    }
    match parse_marker(code, rest) {
        Some(event) => Take::Event(end, event),
        None => Take::Plain(end),
    }
}

fn parse_marker(code: &str, rest: &str) -> Option<IntegrationEvent> {
    let (kind, arg) = rest.split_once(';').unwrap_or((rest, ""));
    let kind = kind.chars().next()?;
    match kind {
        'A' => Some(IntegrationEvent::PromptStart),
        'B' => Some(IntegrationEvent::InputStart),
        'C' => Some(IntegrationEvent::OutputStart),
        'D' => Some(IntegrationEvent::CommandFinished {
            exit: parse_exit(arg),
        }),
        'E' => {
            let raw = if code == "633" {
                arg.split(';').next().unwrap_or("")
            } else {
                arg
            };
            Some(IntegrationEvent::CommandText(unescape_command(raw)))
        }
        _ => None,
    }
}

fn parse_exit(arg: &str) -> ExitCodeField {
    let field = arg.split(';').next().unwrap_or("").trim();
    if field.is_empty() {
        ExitCodeField::Missing
    } else if let Ok(code) = field.parse::<i32>() {
        ExitCodeField::Known(code)
    } else {
        ExitCodeField::Unreadable
    }
}

/// VS Code's OSC 633 escapes a semicolon in the command as `\x3b`.
fn unescape_command(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('\\') => {
                chars.next();
                out.push('\\');
            }
            Some('x' | 'X') => {
                chars.next();
                let high = chars.next();
                let low = chars.next();
                match (high, low) {
                    (Some(high), Some(low)) => {
                        let hex = format!("{high}{low}");
                        match u8::from_str_radix(&hex, 16) {
                            Ok(byte) => out.push(char::from(byte)),
                            Err(_) => {
                                out.push('\\');
                                out.push('x');
                                out.push(high);
                                out.push(low);
                            }
                        }
                    }
                    _ => out.push('\\'),
                }
            }
            _ => out.push('\\'),
        }
    }
    out
}

fn tidy_command(text: &str) -> String {
    text.trim_matches(['\n', '\r']).trim().to_owned()
}

fn tidy_output(text: &str) -> String {
    text.trim_matches(['\n', '\r']).trim_end().to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prompt,
    Input,
    Output,
}

#[derive(Debug)]
struct OpenBlock {
    id: CommandBlockId,
    explicit: Option<String>,
    command: String,
    output: String,
    input_at: Option<GridAnchor>,
    output_at: Option<GridAnchor>,
    phase: Phase,
    started: Option<Instant>,
}

impl OpenBlock {
    fn prompt(id: CommandBlockId) -> Self {
        Self {
            id,
            explicit: None,
            command: String::new(),
            output: String::new(),
            input_at: None,
            output_at: None,
            phase: Phase::Prompt,
            started: None,
        }
    }

    fn snapshot(&self) -> CommandBlock {
        CommandBlock {
            id: self.id,
            command: self.command.clone(),
            output: self.output.clone(),
            status: BlockStatus::Running,
            duration: None,
        }
    }

    fn close(self, status: BlockStatus, duration: Option<Duration>) -> CommandBlock {
        CommandBlock {
            id: self.id,
            command: self.command,
            output: self.output,
            status,
            duration,
        }
    }
}

/// Blocks accumulated from markers. The grid itself is not stored here.
#[derive(Debug, Default)]
pub(crate) struct BlockBook {
    finished: Vec<CommandBlock>,
    open: Option<OpenBlock>,
    next_id: u64,
    seen: bool,
    failed: bool,
}

impl BlockBook {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    /// Forgets a previous integration failure so an explicit retry can listen again.
    ///
    /// Block text and the grid are left as they are.
    pub(crate) fn clear_failure(&mut self) {
        self.failed = false;
    }

    /// Marks the integration unusable and closes a running command as unknown.
    ///
    /// No block is rewritten into a success, and this method has no grid to clear.
    pub(crate) fn fail(&mut self) {
        self.failed = true;
        self.close_open(BlockStatus::Unknown, None);
    }

    pub(crate) fn has_blocks(&self) -> bool {
        !self.finished.is_empty()
            || self
                .open
                .as_ref()
                .is_some_and(|open| open.phase != Phase::Prompt)
    }

    pub(crate) fn input_anchor(&self) -> Option<GridAnchor> {
        self.open.as_ref().and_then(|open| open.input_at)
    }

    pub(crate) fn output_anchor(&self) -> Option<GridAnchor> {
        self.open.as_ref().and_then(|open| open.output_at)
    }

    pub(crate) fn in_input(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.phase == Phase::Input)
    }

    pub(crate) fn in_output(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.phase == Phase::Output)
    }

    pub(crate) fn set_running_output(&mut self, text: Option<String>) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.phase != Phase::Output {
            return;
        }
        if let Some(text) = text {
            open.output = tidy_output(&text);
        }
    }

    pub(crate) fn set_scraped_command(&mut self, text: Option<String>) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.explicit.is_some() {
            return;
        }
        if let Some(text) = text {
            open.command = tidy_command(&text);
        }
    }

    pub(crate) fn prompt_start(&mut self) {
        self.seen = true;
        let previous = self.open.take();
        if let Some(previous) = previous
            && previous.phase != Phase::Prompt
        {
            // A new prompt without a command-end marker is not a successful command.
            self.finished
                .push(previous.close(BlockStatus::Unknown, None));
        }
        self.open = Some(OpenBlock::prompt(self.allocate()));
    }

    pub(crate) fn input_start(&mut self, anchor: GridAnchor) {
        self.seen = true;
        let open = self.ensure_open();
        open.phase = Phase::Input;
        open.input_at = Some(anchor);
    }

    pub(crate) fn set_explicit_command(&mut self, command: String) {
        self.seen = true;
        let open = self.ensure_open();
        let command = tidy_command(&command);
        open.explicit = Some(command.clone());
        open.command = command;
    }

    pub(crate) fn output_start(
        &mut self,
        scraped: Option<String>,
        anchor: GridAnchor,
        now: Instant,
    ) {
        self.seen = true;
        let open = self.ensure_open();
        if let Some(explicit) = open.explicit.clone() {
            open.command = explicit;
        } else if let Some(scraped) = scraped {
            open.command = tidy_command(&scraped);
        }
        open.output_at = Some(anchor);
        open.output.clear();
        open.phase = Phase::Output;
        open.started = Some(now);
    }

    /// Closes the running block because the shell reported a command end.
    ///
    /// Returns whether a command was really ended. An end marker that arrives while
    /// no command had started - at a prompt, or with no block open at all - records
    /// nothing that could be called a finished command, so a caller that marks a
    /// background tab is not told a command ended when none ran (R7).
    pub(crate) fn finish(
        &mut self,
        output: Option<String>,
        exit: ExitCodeField,
        now: Instant,
    ) -> bool {
        self.seen = true;
        if matches!(exit, ExitCodeField::Unreadable) {
            self.failed = true;
        }
        let Some(mut open) = self.open.take() else {
            // An end marker with no command does not invent a successful block.
            return false;
        };
        // A block that never left the prompt has no command to report as ended.
        let ended_a_command = open.phase != Phase::Prompt || !open.command.is_empty();
        if let Some(output) = output {
            open.output = tidy_output(&output);
        }
        let duration = open
            .started
            .map(|started| now.saturating_duration_since(started));
        let status = match exit {
            ExitCodeField::Missing | ExitCodeField::Unreadable => BlockStatus::Unknown,
            ExitCodeField::Known(code) => BlockStatus::Exited(code),
        };
        self.finished.push(open.close(status, duration));
        ended_a_command
    }

    /// Finished blocks, then the running one when a command has actually started.
    pub(crate) fn blocks(&self) -> Vec<CommandBlock> {
        let mut blocks = self.finished.clone();
        if let Some(open) = &self.open
            && open.phase != Phase::Prompt
        {
            blocks.push(open.snapshot());
        }
        blocks
    }

    /// Blocks whose command or output matches a query, limited by `filter`.
    ///
    /// The comparison is the scrollback's own ([`crate::core::SearchOptions`]), so
    /// case and expressions mean the same thing in both places. An empty query keeps
    /// every block the filter allows; an unusable expression is an error, never an
    /// empty list.
    pub(crate) fn search(
        &self,
        query: &str,
        filter: BlockFilter,
        options: SearchOptions,
    ) -> Result<Vec<CommandBlock>, SearchFailure> {
        let matcher = if query.is_empty() {
            None
        } else {
            Some(Matcher::compile(query, options)?)
        };
        Ok(self
            .blocks()
            .into_iter()
            .filter(|block| {
                filter.matches(block.status)
                    && matcher.as_ref().is_none_or(|matcher| {
                        matcher.is_match(&block.command) || matcher.is_match(&block.output)
                    })
            })
            .collect())
    }

    pub(crate) fn get(&self, id: CommandBlockId) -> Option<CommandBlock> {
        self.blocks().into_iter().find(|block| block.id == id)
    }

    fn close_open(&mut self, status: BlockStatus, duration: Option<Duration>) {
        let Some(open) = self.open.take() else {
            return;
        };
        if open.phase == Phase::Prompt && open.command.is_empty() {
            return;
        }
        self.finished.push(open.close(status, duration));
    }

    fn ensure_open(&mut self) -> &mut OpenBlock {
        if self.open.is_none() {
            let id = self.allocate();
            self.open = Some(OpenBlock::prompt(id));
        }
        self.open
            .as_mut()
            .expect("the open block was just inserted")
    }

    fn allocate(&mut self) -> CommandBlockId {
        let id = CommandBlockId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(bytes: &[u8]) -> Vec<Segment> {
        IntegrationSplitter::new().push(bytes)
    }

    fn osc(payload: &str) -> Vec<u8> {
        let mut bytes = b"\x1b]".to_vec();
        bytes.extend_from_slice(payload.as_bytes());
        bytes.push(0x07);
        bytes
    }

    #[test]
    fn plain_text_and_other_osc_sequences_pass_through() {
        let segments = events(b"hello \x1b]0;title\x07\x1b[31m");
        assert_eq!(segments.len(), 1);
        match &segments[0] {
            Segment::Bytes(bytes) => assert_eq!(bytes, b"hello \x1b]0;title\x07\x1b[31m"),
            Segment::Event(_) => panic!("a title OSC is not a command marker"),
        }
    }

    #[test]
    fn osc_133_markers_are_events_and_a_split_marker_is_one_event() {
        let mut splitter = IntegrationSplitter::new();
        let mut first = b"\x1b]13".to_vec();
        assert!(splitter.push(&first).is_empty(), "a cut OSC is held");
        first.clear();
        let rest = b"3;D;0\x07tail";
        let segments = splitter.push(rest);
        assert_eq!(
            segments,
            vec![
                Segment::Event(IntegrationEvent::CommandFinished {
                    exit: ExitCodeField::Known(0),
                }),
                Segment::Bytes(b"tail".to_vec()),
            ]
        );
    }

    #[test]
    fn exit_fields_keep_missing_and_garbage_apart_from_a_real_code() {
        let missing = events(&osc("133;D"));
        let garbage = events(&osc("133;D;nope"));
        let code = events(&osc("133;D;7"));
        assert_eq!(
            missing,
            vec![Segment::Event(IntegrationEvent::CommandFinished {
                exit: ExitCodeField::Missing,
            })]
        );
        assert_eq!(
            garbage,
            vec![Segment::Event(IntegrationEvent::CommandFinished {
                exit: ExitCodeField::Unreadable,
            })]
        );
        assert_eq!(
            code,
            vec![Segment::Event(IntegrationEvent::CommandFinished {
                exit: ExitCodeField::Known(7),
            })]
        );
    }

    #[test]
    fn an_explicit_command_keeps_an_escaped_semicolon() {
        let segments = events(&osc(r"633;E;echo\x3bhi;nonce"));
        assert_eq!(
            segments,
            vec![Segment::Event(IntegrationEvent::CommandText(
                "echo;hi".to_owned()
            ))]
        );
    }

    #[test]
    fn st_terminated_markers_match_bel_terminated_ones() {
        let bytes = b"\x1b]133;A\x1b\\";
        assert_eq!(
            events(bytes),
            vec![Segment::Event(IntegrationEvent::PromptStart)]
        );
    }

    #[test]
    fn unknown_is_not_success_and_only_a_zero_code_is() {
        assert!(!BlockStatus::Running.is_success());
        assert!(!BlockStatus::Unknown.is_success());
        assert!(!BlockStatus::Exited(1).is_success());
        assert!(BlockStatus::Exited(0).is_success());
        assert!(!BlockFilter::ExitedZero.matches(BlockStatus::Unknown));
        assert!(!BlockFilter::ExitedZero.matches(BlockStatus::Running));
        assert!(BlockFilter::ExitedZero.matches(BlockStatus::Exited(0)));
    }

    #[test]
    fn only_an_end_marker_that_closes_a_command_reports_one() {
        let mut book = BlockBook::new();
        let now = Instant::now();
        let anchor = GridAnchor {
            stable: 0,
            column: 0,
        };
        // Nothing was running, so an end marker reports no completion.
        assert!(!book.finish(None, ExitCodeField::Known(0), now));

        book.prompt_start();
        book.input_start(anchor);
        book.set_explicit_command("true".to_owned());
        book.output_start(None, anchor, now);
        assert!(
            book.finish(None, ExitCodeField::Known(0), now),
            "a running command the shell ended is one reported completion"
        );

        // A new prompt without an end marker closes the block as unknown, and that is
        // not something the shell reported as ended.
        book.prompt_start();
        book.input_start(anchor);
        book.set_explicit_command("sleep 5".to_owned());
        book.output_start(None, anchor, now);
        book.prompt_start();
        // Nor is an end marker that arrives at a bare prompt: no command ran.
        assert!(!book.finish(None, ExitCodeField::Missing, now));
        assert_eq!(
            book.blocks()
                .into_iter()
                .filter(|block| !block.command.is_empty())
                .map(|block| block.status)
                .collect::<Vec<_>>(),
            vec![BlockStatus::Exited(0), BlockStatus::Unknown],
            "the reported end kept its code, and the interrupted command stayed unknown"
        );
    }
}
