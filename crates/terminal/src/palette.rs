//! Colour resolution for the terminal grid (R8).
//!
//! The terminal state keeps colours in the three forms a program can ask for —
//! named ANSI slots, 256-colour indices and direct RGB — and leaves resolution to
//! the renderer. This module turns every one of those into concrete RGB values so
//! the view never has to know how a colour was requested.
//!
//! Two sources are consulted in order: overrides the program itself set with OSC
//! 4/10/11/12, then the palette below. That order matters — an application that
//! redefines a slot expects its own colour, not ours.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// Default text colour, matching the shell's primary text token.
pub const FOREGROUND: Rgb = Rgb {
    r: 0xee,
    g: 0xf1,
    b: 0xf7,
};

/// Default terminal background, matching the shell canvas token.
pub const BACKGROUND: Rgb = Rgb {
    r: 0x0b,
    g: 0x0d,
    b: 0x12,
};

/// Default cursor colour, matching the accent token.
pub const CURSOR: Rgb = Rgb {
    r: 0x9d,
    g: 0xb4,
    b: 0xff,
};

/// The sixteen ANSI colours.
///
/// They are chosen for a dark canvas and checked for contrast in the tests: every
/// colour a program can use as text stays readable on `BACKGROUND`, and the bright
/// variants are visibly lighter rather than merely different.
pub const ANSI: [Rgb; 16] = [
    Rgb {
        r: 0x2b,
        g: 0x31,
        b: 0x3d,
    },
    Rgb {
        r: 0xff,
        g: 0x8d,
        b: 0x99,
    },
    Rgb {
        r: 0x64,
        g: 0xd8,
        b: 0xa8,
    },
    Rgb {
        r: 0xf3,
        g: 0xc6,
        b: 0x6c,
    },
    Rgb {
        r: 0x9d,
        g: 0xb4,
        b: 0xff,
    },
    Rgb {
        r: 0xd7,
        g: 0xa0,
        b: 0xff,
    },
    Rgb {
        r: 0x7f,
        g: 0xd8,
        b: 0xe8,
    },
    Rgb {
        r: 0xb5,
        g: 0xbd,
        b: 0xcc,
    },
    Rgb {
        r: 0x82,
        g: 0x8b,
        b: 0x9c,
    },
    Rgb {
        r: 0xff,
        g: 0xb3,
        b: 0xbc,
    },
    Rgb {
        r: 0x8d,
        g: 0xe6,
        b: 0xc0,
    },
    Rgb {
        r: 0xf8,
        g: 0xd8,
        b: 0x94,
    },
    Rgb {
        r: 0xbc,
        g: 0xcb,
        b: 0xff,
    },
    Rgb {
        r: 0xe3,
        g: 0xc4,
        b: 0xff,
    },
    Rgb {
        r: 0xa8,
        g: 0xe9,
        b: 0xf4,
    },
    Rgb {
        r: 0xee,
        g: 0xf1,
        b: 0xf7,
    },
];

/// Resolves any cell colour against the current overrides.
#[must_use]
pub fn resolve(color: Color, colors: &Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(index) => colors[index as usize].unwrap_or_else(|| indexed(index)),
        Color::Named(named) => colors[named].unwrap_or_else(|| named_color(named)),
    }
}

/// The palette entry for a named colour.
///
/// The variants are matched by name rather than by discriminant: the numbering is
/// an implementation detail of the terminal model, and a colour that silently
/// resolved to the wrong slot would be a rendering bug nobody notices until a
/// program uses it.
#[must_use]
pub fn named_color(color: NamedColor) -> Rgb {
    match color {
        NamedColor::Black => ANSI[0],
        NamedColor::Red => ANSI[1],
        NamedColor::Green => ANSI[2],
        NamedColor::Yellow => ANSI[3],
        NamedColor::Blue => ANSI[4],
        NamedColor::Magenta => ANSI[5],
        NamedColor::Cyan => ANSI[6],
        NamedColor::White => ANSI[7],
        NamedColor::BrightBlack => ANSI[8],
        NamedColor::BrightRed => ANSI[9],
        NamedColor::BrightGreen => ANSI[10],
        NamedColor::BrightYellow => ANSI[11],
        NamedColor::BrightBlue => ANSI[12],
        NamedColor::BrightMagenta => ANSI[13],
        NamedColor::BrightCyan => ANSI[14],
        NamedColor::BrightWhite => ANSI[15],
        NamedColor::Background => BACKGROUND,
        NamedColor::Cursor => CURSOR,
        // The dim variants exist so a program can ask for "regular but dimmer"
        // without knowing the palette; halving the regular colour is what the
        // reference terminals do.
        NamedColor::DimBlack => dim(ANSI[0]),
        NamedColor::DimRed => dim(ANSI[1]),
        NamedColor::DimGreen => dim(ANSI[2]),
        NamedColor::DimYellow => dim(ANSI[3]),
        NamedColor::DimBlue => dim(ANSI[4]),
        NamedColor::DimMagenta => dim(ANSI[5]),
        NamedColor::DimCyan => dim(ANSI[6]),
        NamedColor::DimWhite => dim(ANSI[7]),
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            FOREGROUND
        }
    }
}

/// The colour for a 256-colour index.
///
/// Indices 0-15 are the ANSI palette, 16-231 the 6×6×6 cube and 232-255 the
/// grey ramp; this is the layout xterm defines, so a program that picks index 196
/// gets the same red strip everywhere.
#[must_use]
pub fn indexed(index: u8) -> Rgb {
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let index = index - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            Rgb {
                r: steps[(index / 36) as usize],
                g: steps[((index % 36) / 6) as usize],
                b: steps[(index % 6) as usize],
            }
        }
        _ => {
            let level = 8 + (index - 232) * 10;
            Rgb {
                r: level,
                g: level,
                b: level,
            }
        }
    }
}

/// The same hue at roughly half the luminance.
fn dim(color: Rgb) -> Rgb {
    Rgb {
        r: color.r / 2,
        g: color.g / 2,
        b: color.b / 2,
    }
}

/// The CSS style value for a colour, used when a view needs a string.
#[must_use]
pub fn to_hex(color: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Rgb) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn every_ansi_colour_is_readable_on_the_terminal_background() {
        for (index, color) in ANSI.iter().enumerate() {
            let ratio = contrast(*color, BACKGROUND);
            // Slot 0 is the "black" a program paints backgrounds with; it is not
            // expected to be readable as text, so it is checked separately.
            if index == 0 {
                assert!(ratio < 4.5, "slot 0 must stay a dark background colour");
                continue;
            }
            assert!(
                ratio >= 4.5,
                "ANSI colour {index} is {ratio:.2}:1 on the terminal background"
            );
        }
    }

    #[test]
    fn bright_slots_are_lighter_than_the_regular_ones() {
        for slot in 0..8 {
            let regular = luminance(ANSI[slot]);
            let bright = luminance(ANSI[slot + 8]);
            assert!(
                bright > regular,
                "bright slot {slot} is not lighter than its regular counterpart"
            );
        }
    }

    #[test]
    fn direct_rgb_wins_over_the_palette() {
        let colors = Colors::default();
        let rgb = Rgb { r: 1, g: 2, b: 3 };
        assert_eq!(resolve(Color::Spec(rgb), &colors), rgb);
    }

    #[test]
    fn an_override_set_by_the_program_takes_precedence() {
        let mut colors = Colors::default();
        let override_colour = Rgb {
            r: 0x10,
            g: 0x20,
            b: 0x30,
        };
        colors[NamedColor::Red] = Some(override_colour);
        colors[4usize] = Some(override_colour);

        assert_eq!(
            resolve(Color::Named(NamedColor::Red), &colors),
            override_colour
        );
        assert_eq!(resolve(Color::Indexed(4), &colors), override_colour);
        // A slot the program did not touch still comes from our palette.
        assert_eq!(resolve(Color::Named(NamedColor::Green), &colors), ANSI[2]);
    }

    #[test]
    fn the_256_colour_cube_follows_the_xterm_layout() {
        assert_eq!(indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(indexed(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(indexed(21), Rgb { r: 0, g: 0, b: 255 });
        // The grey ramp starts at 8 and steps by 10.
        assert_eq!(indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            indexed(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
        // Indices below 16 are the named palette, not cube entries.
        assert_eq!(indexed(2), ANSI[2]);
        assert_eq!(indexed(11), ANSI[11]);
    }

    #[test]
    fn foreground_background_and_cursor_have_distinct_defaults() {
        assert_eq!(named_color(NamedColor::Foreground), FOREGROUND);
        assert_eq!(named_color(NamedColor::Background), BACKGROUND);
        assert_eq!(named_color(NamedColor::Cursor), CURSOR);
        assert_ne!(FOREGROUND, BACKGROUND);
        assert_ne!(CURSOR, BACKGROUND);
        assert_eq!(named_color(NamedColor::BrightForeground), FOREGROUND);
        assert_eq!(named_color(NamedColor::DimForeground), FOREGROUND);
    }

    #[test]
    fn dim_colours_are_darker_than_their_regular_counterparts() {
        let pairs = [
            (NamedColor::Black, NamedColor::DimBlack),
            (NamedColor::Red, NamedColor::DimRed),
            (NamedColor::Green, NamedColor::DimGreen),
            (NamedColor::Yellow, NamedColor::DimYellow),
            (NamedColor::Blue, NamedColor::DimBlue),
            (NamedColor::Magenta, NamedColor::DimMagenta),
            (NamedColor::Cyan, NamedColor::DimCyan),
            (NamedColor::White, NamedColor::DimWhite),
        ];
        for (regular, dimmed) in pairs {
            assert!(
                luminance(named_color(dimmed)) < luminance(named_color(regular)),
                "{dimmed:?} is not darker than {regular:?}"
            );
        }
    }

    #[test]
    fn hex_output_matches_the_colour() {
        assert_eq!(
            to_hex(Rgb {
                r: 0x0b,
                g: 0x0d,
                b: 0x12
            }),
            "#0b0d12"
        );
    }
}
