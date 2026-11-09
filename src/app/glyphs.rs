//! Box-drawing and block characters, drawn as shapes rather than glyphs.
//!
//! Terminal programs build frames, rules, progress bars and logos from the box
//! drawing (U+2500–U+257F) and block element (U+2580–U+259F) ranges. Drawn from a
//! font, each of them comes from whichever face has it, at that face's width and
//! height: lines break into dashes between cells, frames drift off their columns
//! and blocks leave seams. Drawn here, every one fills exactly its cell, so lines
//! join their neighbours and blocks tile.
//!
//! This module only describes the shapes, in fractions of a cell; the terminal
//! view turns them into pixels.

/// How heavy one arm of a box-drawing character is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    /// No arm on this side.
    None,
    /// A thin line.
    Light,
    /// A line twice as thick.
    Heavy,
    /// Two thin parallel lines.
    Double,
}

/// The four arms of a box-drawing character, each from the centre of the cell to
/// one of its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arms {
    pub left: Weight,
    pub right: Weight,
    pub up: Weight,
    pub down: Weight,
}

/// A corner drawn as a quarter circle, from one edge to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arc {
    /// ╭: from the right edge round to the bottom edge.
    DownRight,
    /// ╮: from the left edge round to the bottom edge.
    DownLeft,
    /// ╯: from the left edge round to the top edge.
    UpLeft,
    /// ╰: from the right edge round to the top edge.
    UpRight,
}

/// A filled part of a cell, in fractions of its width and height from the top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

/// What a character draws when the renderer draws it itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// Lines from the centre to the edges.
    Lines(Arms),
    /// A rounded corner.
    Arc(Arc),
    /// Up to three filled parts of the cell, at an opacity (the shades are the
    /// full cell at a quarter, a half and three quarters).
    Fill {
        rects: [Option<Rect>; 3],
        alpha: f32,
    },
}

impl Shape {
    /// Whether a row of the same shape side by side draws one continuous shape, so
    /// the row can be drawn as one piece: a horizontal rule, or a block that spans
    /// the whole width of its cell.
    #[must_use]
    pub fn tiles_horizontally(&self) -> bool {
        match self {
            Self::Lines(arms) => {
                arms.up == Weight::None && arms.down == Weight::None && arms.left == arms.right
            }
            Self::Arc(_) => false,
            Self::Fill { rects, .. } => rects
                .iter()
                .flatten()
                .all(|rect| rect.left == 0.0 && rect.right == 1.0),
        }
    }
}

/// The shape a character draws, or `None` for a character the font draws.
#[must_use]
pub fn shape(character: char) -> Option<Shape> {
    let code = u32::from(character);
    match code {
        0x2500..=0x256C | 0x2574..=0x257F => arms(code).map(Shape::Lines),
        0x256D => Some(Shape::Arc(Arc::DownRight)),
        0x256E => Some(Shape::Arc(Arc::DownLeft)),
        0x256F => Some(Shape::Arc(Arc::UpLeft)),
        0x2570 => Some(Shape::Arc(Arc::UpRight)),
        0x2580..=0x259F => block(code),
        _ => None,
    }
}

fn arms(code: u32) -> Option<Arms> {
    use Weight::{Double as D, Heavy as H, Light as L, None as N};
    // (left, right, up, down)
    let (left, right, up, down) = match code {
        0x2500 | 0x2504 | 0x2508 | 0x254C => (L, L, N, N),
        0x2501 | 0x2505 | 0x2509 | 0x254D => (H, H, N, N),
        0x2502 | 0x2506 | 0x250A | 0x254E => (N, N, L, L),
        0x2503 | 0x2507 | 0x250B | 0x254F => (N, N, H, H),
        0x250C => (N, L, N, L),
        0x250D => (N, H, N, L),
        0x250E => (N, L, N, H),
        0x250F => (N, H, N, H),
        0x2510 => (L, N, N, L),
        0x2511 => (H, N, N, L),
        0x2512 => (L, N, N, H),
        0x2513 => (H, N, N, H),
        0x2514 => (N, L, L, N),
        0x2515 => (N, H, L, N),
        0x2516 => (N, L, H, N),
        0x2517 => (N, H, H, N),
        0x2518 => (L, N, L, N),
        0x2519 => (H, N, L, N),
        0x251A => (L, N, H, N),
        0x251B => (H, N, H, N),
        0x251C => (N, L, L, L),
        0x251D => (N, H, L, L),
        0x251E => (N, L, H, L),
        0x251F => (N, L, L, H),
        0x2520 => (N, L, H, H),
        0x2521 => (N, H, H, L),
        0x2522 => (N, H, L, H),
        0x2523 => (N, H, H, H),
        0x2524 => (L, N, L, L),
        0x2525 => (H, N, L, L),
        0x2526 => (L, N, H, L),
        0x2527 => (L, N, L, H),
        0x2528 => (L, N, H, H),
        0x2529 => (H, N, H, L),
        0x252A => (H, N, L, H),
        0x252B => (H, N, H, H),
        0x252C => (L, L, N, L),
        0x252D => (H, L, N, L),
        0x252E => (L, H, N, L),
        0x252F => (H, H, N, L),
        0x2530 => (L, L, N, H),
        0x2531 => (H, L, N, H),
        0x2532 => (L, H, N, H),
        0x2533 => (H, H, N, H),
        0x2534 => (L, L, L, N),
        0x2535 => (H, L, L, N),
        0x2536 => (L, H, L, N),
        0x2537 => (H, H, L, N),
        0x2538 => (L, L, H, N),
        0x2539 => (H, L, H, N),
        0x253A => (L, H, H, N),
        0x253B => (H, H, H, N),
        0x253C => (L, L, L, L),
        0x253D => (H, L, L, L),
        0x253E => (L, H, L, L),
        0x253F => (H, H, L, L),
        0x2540 => (L, L, H, L),
        0x2541 => (L, L, L, H),
        0x2542 => (L, L, H, H),
        0x2543 => (H, L, H, L),
        0x2544 => (L, H, H, L),
        0x2545 => (H, L, L, H),
        0x2546 => (L, H, L, H),
        0x2547 => (H, H, H, L),
        0x2548 => (H, H, L, H),
        0x2549 => (H, L, H, H),
        0x254A => (L, H, H, H),
        0x254B => (H, H, H, H),
        0x2550 => (D, D, N, N),
        0x2551 => (N, N, D, D),
        0x2552 => (N, D, N, L),
        0x2553 => (N, L, N, D),
        0x2554 => (N, D, N, D),
        0x2555 => (D, N, N, L),
        0x2556 => (L, N, N, D),
        0x2557 => (D, N, N, D),
        0x2558 => (N, D, L, N),
        0x2559 => (N, L, D, N),
        0x255A => (N, D, D, N),
        0x255B => (D, N, L, N),
        0x255C => (L, N, D, N),
        0x255D => (D, N, D, N),
        0x255E => (N, D, L, L),
        0x255F => (N, L, D, D),
        0x2560 => (N, D, D, D),
        0x2561 => (D, N, L, L),
        0x2562 => (L, N, D, D),
        0x2563 => (D, N, D, D),
        0x2564 => (D, D, N, L),
        0x2565 => (L, L, N, D),
        0x2566 => (D, D, N, D),
        0x2567 => (D, D, L, N),
        0x2568 => (L, L, D, N),
        0x2569 => (D, D, D, N),
        0x256A => (D, D, L, L),
        0x256B => (L, L, D, D),
        0x256C => (D, D, D, D),
        0x2574 => (L, N, N, N),
        0x2575 => (N, N, L, N),
        0x2576 => (N, L, N, N),
        0x2577 => (N, N, N, L),
        0x2578 => (H, N, N, N),
        0x2579 => (N, N, H, N),
        0x257A => (N, H, N, N),
        0x257B => (N, N, N, H),
        0x257C => (L, H, N, N),
        0x257D => (N, N, L, H),
        0x257E => (H, L, N, N),
        0x257F => (N, N, H, L),
        _ => return None,
    };
    Some(Arms {
        left,
        right,
        up,
        down,
    })
}

fn block(code: u32) -> Option<Shape> {
    let eighth = |n: u32| n as f32 / 8.0;
    let one = |rect: Rect| Shape::Fill {
        rects: [Some(rect), None, None],
        alpha: 1.0,
    };
    let quadrants = |parts: &[(f32, f32)]| {
        let mut rects = [None; 3];
        for (slot, (left, top)) in rects.iter_mut().zip(parts) {
            *slot = Some(Rect::new(*left, *top, left + 0.5, top + 0.5));
        }
        Shape::Fill { rects, alpha: 1.0 }
    };
    let shade = |alpha: f32| Shape::Fill {
        rects: [Some(Rect::new(0.0, 0.0, 1.0, 1.0)), None, None],
        alpha,
    };
    Some(match code {
        0x2580 => one(Rect::new(0.0, 0.0, 1.0, 0.5)),
        // ▁ to ▇: the lower eighths, and █ the whole cell.
        0x2581..=0x2588 => one(Rect::new(0.0, 1.0 - eighth(code - 0x2580), 1.0, 1.0)),
        // ▉ to ▏: the left seven eighths down to one.
        0x2589..=0x258F => one(Rect::new(0.0, 0.0, eighth(0x2590 - code), 1.0)),
        0x2590 => one(Rect::new(0.5, 0.0, 1.0, 1.0)),
        0x2591 => shade(0.25),
        0x2592 => shade(0.5),
        0x2593 => shade(0.75),
        0x2594 => one(Rect::new(0.0, 0.0, 1.0, 0.125)),
        0x2595 => one(Rect::new(0.875, 0.0, 1.0, 1.0)),
        0x2596 => quadrants(&[(0.0, 0.5)]),
        0x2597 => quadrants(&[(0.5, 0.5)]),
        0x2598 => quadrants(&[(0.0, 0.0)]),
        0x2599 => quadrants(&[(0.0, 0.0), (0.0, 0.5), (0.5, 0.5)]),
        0x259A => quadrants(&[(0.0, 0.0), (0.5, 0.5)]),
        0x259B => quadrants(&[(0.0, 0.0), (0.5, 0.0), (0.0, 0.5)]),
        0x259C => quadrants(&[(0.0, 0.0), (0.5, 0.0), (0.5, 0.5)]),
        0x259D => quadrants(&[(0.5, 0.0)]),
        0x259E => quadrants(&[(0.5, 0.0), (0.0, 0.5)]),
        0x259F => quadrants(&[(0.5, 0.0), (0.0, 0.5), (0.5, 0.5)]),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_of_both_ranges_has_a_shape_except_the_diagonals() {
        for code in 0x2500..=0x259F_u32 {
            let character = char::from_u32(code).expect("a valid scalar");
            let diagonal = (0x2571..=0x2573).contains(&code);
            assert_eq!(
                shape(character).is_some(),
                !diagonal,
                "U+{code:04X} {character}"
            );
        }
    }

    #[test]
    fn rules_and_full_blocks_tile_and_corners_do_not() {
        assert!(shape('─').expect("a rule").tiles_horizontally());
        assert!(shape('═').expect("a rule").tiles_horizontally());
        assert!(shape('█').expect("a block").tiles_horizontally());
        assert!(shape('▄').expect("a block").tiles_horizontally());
        assert!(shape('░').expect("a shade").tiles_horizontally());
        assert!(!shape('┌').expect("a corner").tiles_horizontally());
        assert!(!shape('╭').expect("an arc").tiles_horizontally());
        assert!(!shape('▌').expect("a half").tiles_horizontally());
        assert!(!shape('│').expect("a bar").tiles_horizontally());
    }

    #[test]
    fn the_eighths_grow_in_order() {
        let height = |character: char| match shape(character) {
            Some(Shape::Fill { rects, .. }) => {
                let rect = rects[0].expect("one rect");
                rect.bottom - rect.top
            }
            other => panic!("{character} is {other:?}"),
        };
        assert!((height('▁') - 0.125).abs() < f32::EPSILON);
        assert!((height('▄') - 0.5).abs() < f32::EPSILON);
        assert!((height('█') - 1.0).abs() < f32::EPSILON);
        assert!(shape('a').is_none());
        assert!(shape('╱').is_none());
    }
}
