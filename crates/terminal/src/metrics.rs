//! Cell metrics, grid fitting and font size steps (R8).
//!
//! The PTY, the terminal grid and the view have to agree on how many columns and
//! rows a given window area holds: the grid decides where text wraps, and the PTY
//! decides where the shell wraps. Both are told the same number, and that number
//! is computed here — once, from the same cell size — instead of being guessed in
//! the view.
//!
//! Font size is a user preference with a hard floor and ceiling: the grid never
//! becomes zero cells wide, and text cannot be shrunk to something unreadable.

use alacritty_terminal::grid::Dimensions;

/// Font size a new terminal starts with.
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

/// Smallest font size the controls reach.
pub const MIN_FONT_SIZE: f32 = 9.0;

/// Largest font size the controls reach.
pub const MAX_FONT_SIZE: f32 = 28.0;

/// One press of the size control.
pub const FONT_SIZE_STEP: f32 = 1.0;

/// Narrowest grid that can still show a prompt.
pub const MIN_COLUMNS: usize = 2;

/// Shortest grid that can still show a line.
pub const MIN_ROWS: usize = 1;

/// Advance of one cell relative to the font size.
///
/// A monospace face advances by roughly 0.6em; the exact value is measured from
/// the font when a window is available, and this ratio is the fallback for tests
/// and for the first frame.
pub const MONOSPACE_ADVANCE_RATIO: f32 = 0.6;

/// Line box height relative to the font size.
pub const LINE_HEIGHT_RATIO: f32 = 1.25;

/// Size of one character cell in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellSize {
    /// Horizontal advance.
    pub width: f32,
    /// Line height.
    pub height: f32,
}

impl CellSize {
    /// The size a font size implies when the advance is not measured.
    #[must_use]
    pub fn for_font_size(font_size: f32) -> Self {
        Self {
            width: font_size * MONOSPACE_ADVANCE_RATIO,
            height: font_size * LINE_HEIGHT_RATIO,
        }
    }

    /// The same cell with non-positive values replaced, so a caller can divide.
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            width: if self.width > 0.0 { self.width } else { 1.0 },
            height: if self.height > 0.0 { self.height } else { 1.0 },
        }
    }
}

impl Default for CellSize {
    fn default() -> Self {
        Self::for_font_size(DEFAULT_FONT_SIZE)
    }
}

/// Grid dimensions: how many cells the terminal has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    /// Columns, at least `MIN_COLUMNS`.
    pub cols: usize,
    /// Rows, at least `MIN_ROWS`.
    pub rows: usize,
}

impl GridSize {
    /// A grid clamped to the smallest usable size.
    #[must_use]
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols: cols.max(MIN_COLUMNS),
            rows: rows.max(MIN_ROWS),
        }
    }

    /// The size a terminal starts with before a window has been measured.
    #[must_use]
    pub fn initial() -> Self {
        Self { cols: 80, rows: 24 }
    }

    /// The same grid with both dimensions clamped.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self::new(self.cols, self.rows)
    }

    /// Total cells, used by the view to size what it draws.
    #[must_use]
    pub fn cells(self) -> usize {
        self.cols * self.rows
    }
}

impl Default for GridSize {
    fn default() -> Self {
        Self::initial()
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// How many cells fit in an area, rounded down.
///
/// Rounding down is the important part: a partial cell is not usable, and telling
/// the PTY about a column the view cannot draw would put the shell cursor outside
/// the visible grid.
#[must_use]
pub fn grid_for(available_width: f32, available_height: f32, cell: CellSize) -> GridSize {
    let cell = cell.sanitized();
    if !available_width.is_finite() || !available_height.is_finite() {
        return GridSize::initial();
    }
    let cols = (available_width / cell.width).floor().max(0.0) as usize;
    let rows = (available_height / cell.height).floor().max(0.0) as usize;
    GridSize::new(cols, rows)
}

/// The font size one step up or down, clamped to the allowed range.
#[must_use]
pub fn step_font_size(current: f32, larger: bool) -> f32 {
    let current = sanitize_font_size(current);
    let next = if larger {
        current + FONT_SIZE_STEP
    } else {
        current - FONT_SIZE_STEP
    };
    next.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// A font size brought into the allowed range.
#[must_use]
pub fn sanitize_font_size(size: f32) -> f32 {
    if !size.is_finite() {
        return DEFAULT_FONT_SIZE;
    }
    size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_never_collapses_to_nothing() {
        assert_eq!(
            GridSize::new(0, 0),
            GridSize {
                cols: MIN_COLUMNS,
                rows: MIN_ROWS
            }
        );
        let tiny = grid_for(1.0, 1.0, CellSize::for_font_size(DEFAULT_FONT_SIZE));
        assert_eq!(tiny.cols, MIN_COLUMNS);
        assert_eq!(tiny.rows, MIN_ROWS);
    }

    #[test]
    fn a_partial_cell_is_not_counted_as_a_column() {
        let cell = CellSize {
            width: 10.0,
            height: 20.0,
        };
        let grid = grid_for(109.0, 99.0, cell);
        assert_eq!(grid.cols, 10, "109px holds ten 10px cells, not eleven");
        assert_eq!(grid.rows, 4, "99px holds four 20px lines");
    }

    #[test]
    fn the_grid_grows_and_shrinks_with_the_window() {
        let cell = CellSize::for_font_size(DEFAULT_FONT_SIZE);
        let small = grid_for(800.0, 600.0, cell);
        let large = grid_for(1600.0, 1200.0, cell);
        assert!(large.cols > small.cols);
        assert!(large.rows > small.rows);
    }

    #[test]
    fn a_larger_font_leaves_room_for_fewer_columns() {
        let small_font = grid_for(800.0, 600.0, CellSize::for_font_size(12.0));
        let large_font = grid_for(800.0, 600.0, CellSize::for_font_size(20.0));
        assert!(large_font.cols < small_font.cols);
        assert!(large_font.rows < small_font.rows);
    }

    #[test]
    fn font_size_steps_stay_inside_the_range() {
        assert_eq!(step_font_size(MIN_FONT_SIZE, false), MIN_FONT_SIZE);
        assert_eq!(step_font_size(MAX_FONT_SIZE, true), MAX_FONT_SIZE);
        assert_eq!(step_font_size(20.0, true), 21.0);
        assert_eq!(step_font_size(20.0, false), 19.0);
        assert_eq!(
            step_font_size(f32::NAN, true),
            DEFAULT_FONT_SIZE + FONT_SIZE_STEP
        );
    }

    #[test]
    fn an_infinite_layout_falls_back_to_the_initial_grid() {
        let grid = grid_for(f32::INFINITY, f32::INFINITY, CellSize::default());
        assert_eq!(grid, GridSize::initial());
    }

    #[test]
    fn the_grid_reports_the_dimensions_the_terminal_needs() {
        let grid = GridSize::new(120, 40);
        assert_eq!(grid.columns(), 120);
        assert_eq!(grid.screen_lines(), 40);
        assert_eq!(grid.total_lines(), 40);
        assert_eq!(grid.cells(), 4800);
    }
}
