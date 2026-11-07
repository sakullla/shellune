//! Terminal state, input encoding and the local PTY for Shellune (R2, R8).
//!
//! The crate exists so that the risky parts of a terminal — escape sequence
//! parsing, colour resolution, cursor placement, selection, scrollback, resize and
//! the PTY process itself — can be tested without a window, and so that the local
//! and (later) remote terminals share one implementation instead of drifting apart.
//!
//! * [`core`] owns the grid and turns bytes into a snapshot a view can draw;
//! * [`blocks`] records command blocks from OSC 133 / OSC 633 markers only;
//! * [`input`] turns keys and pasted text into the bytes a program reads;
//! * [`metrics`] decides how many cells fit in an area and how large the font may be;
//! * [`palette`] resolves named, indexed and direct colours;
//! * [`pty`] starts a real shell and reports its output, its size changes and its
//!   exit status;
//! * [`shell`] lists the shells that actually exist on this machine.
//!
//! Nothing here depends on GPUI, so a failure in this crate is a terminal problem
//! and never a rendering problem.

pub mod blocks;
pub mod core;
pub mod input;
pub mod metrics;
pub mod palette;
pub mod pty;
pub mod shell;

pub use alacritty_terminal::selection::SelectionType;
pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
pub use blocks::{BlockFilter, BlockStatus, CommandBlock, CommandBlockId};
pub use core::{
    CellAttributes, DEFAULT_SCROLLBACK, HistoryMatch, RenderedCell, RenderedCursor, RenderedRow,
    Snapshot, TerminalCore, TerminalEffects,
};
pub use input::{
    Key, KeyModifiers, encode_key, encode_paste, encode_text, paste_needs_confirmation,
    paste_preview,
};
pub use metrics::{
    CellSize, DEFAULT_FONT_SIZE, FONT_SIZE_STEP, GridSize, LINE_HEIGHT_RATIO, MAX_FONT_SIZE,
    MIN_FONT_SIZE, MONOSPACE_ADVANCE_RATIO, grid_for, sanitize_font_size, step_font_size,
};
pub use palette::{ANSI, BACKGROUND, CURSOR, FOREGROUND, indexed, named_color, resolve, to_hex};
pub use pty::{ExitReport, LocalSession, PtyEvent, ShellLaunchError, TERM_VALUE};
pub use shell::{discover, discover_with, platform_default, program_exists};
