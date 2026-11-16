//! Design tokens for the Shellune shell (R1, R12, R13).
//!
//! The values here are the single source of truth for the palette, the surface layers,
//! elevation, motion, spacing, corner radii, the type scale and zoom behaviour described
//! in `docs/design/02-shell-and-design-system.md`. Views name tokens instead of literal
//! colours, shadows, durations or sizes, so a visual review only has to change this
//! module.
//!
//! The two palettes are two tables, [`dark`] and [`light`], written side by side; a
//! [`Token`] carries one value from each. Neither is derived from the other, so a light
//! colour is chosen for a light window and not looked up from a dark one.
//!
//! Four properties are checked by tests rather than trusted:
//!
//! * every text and status colour reaches the WCAG contrast floor against every surface
//!   it is drawn on ([`contrast`]), in both palettes;
//! * the five surface layers step in one direction, so a raised object is told apart
//!   from what it floats over even before its shadow is drawn;
//! * the chrome stays neutral and the accent is the only identity hue in regular use,
//!   which is what keeps two themes looking like one product;
//! * layout metrics shrink and grow with [`Zoom`] instead of being hard-coded per view.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, BoxShadow, ElementId, FontWeight, IntoElement, Pixels,
    Rgba, Styled, point, px, rgb,
};
use shellune_core::{SessionFailure, SessionKind, SessionState, TabAccent, TransferState};

/// One colour of the interface: its value in the dark palette and in the light one.
///
/// Views hold tokens, never resolved colours; [`swatch`] resolves one in the palette the
/// window is being drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Token {
    dark: u32,
    light: u32,
}

impl Token {
    /// A token from its dark and its light value.
    #[must_use]
    pub const fn new(dark: u32, light: u32) -> Self {
        Self { dark, light }
    }

    /// The 24-bit sRGB value in the dark palette.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn dark(self) -> u32 {
        self.dark
    }

    /// The 24-bit sRGB value in the light palette.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn light(self) -> u32 {
        self.light
    }

    /// The value in one palette.
    #[must_use]
    pub const fn value(self, appearance: Appearance) -> u32 {
        match appearance {
            Appearance::Dark => self.dark,
            Appearance::Light => self.light,
        }
    }
}

impl std::fmt::LowerHex for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:06x}/{:06x}", self.dark, self.light)
    }
}

/// The dark palette: a blue-black graphite.
///
/// The surfaces climb in five layers, each a little lighter than the one it sits on:
/// canvas, chrome, surface, raised and overlay. A dark palette cannot lean on shadow
/// alone - a shadow on black barely shows - so a raised object also carries a lighter
/// fill and a brighter edge.
// The tables hold every token, including the ones only the next screens draw with.
#[allow(dead_code)]
pub mod dark {
    /// The window's floor: behind the content area and around the terminal.
    pub const CANVAS: u32 = 0x0d0e12;
    /// The title bar, the tab strip, the navigation rail and the status bar.
    pub const CHROME: u32 = 0x131419;
    /// Content: cards, list rows, inputs and the current tab.
    pub const SURFACE: u32 = 0x18191f;
    /// Things that float over content: menus, popovers, tooltips and toasts.
    pub const RAISED: u32 = 0x1f2027;
    /// Dialogs, the front-most layer.
    pub const OVERLAY: u32 = 0x23242c;
    pub const SURFACE_HOVER: u32 = 0x262730;
    pub const SURFACE_SELECTED: u32 = 0x2b2c36;
    pub const SURFACE_PRESSED: u32 = 0x31323d;
    pub const BORDER: u32 = 0x272831;
    pub const BORDER_STRONG: u32 = 0x373844;
    pub const SEGMENT_TRACK: u32 = 0x101115;
    pub const SEGMENT_THUMB: u32 = 0x2c2d37;
    pub const TEXT_PRIMARY: u32 = 0xeeeff4;
    pub const TEXT_SECONDARY: u32 = 0xb6b8c3;
    pub const TEXT_MUTED: u32 = 0x9c9faa;
    pub const ACCENT: u32 = 0x8f9dff;
    pub const ACCENT_HOVER: u32 = 0xa6b1ff;
    pub const ACCENT_PRESSED: u32 = 0x7887f2;
    pub const ON_ACCENT: u32 = 0x0d1033;
    pub const ACCENT_TINT: u32 = 0x1d2042;
    pub const ACCENT_ON_TINT: u32 = 0xc0c8ff;
    pub const POSITIVE: u32 = 0x5fd4a3;
    pub const WARNING: u32 = 0xe9c27c;
    pub const DANGER: u32 = 0xff8f97;
    pub const INFO: u32 = 0x7cc4f2;
    pub const PLANNED: u32 = 0x9ca1ad;
    pub const POSITIVE_TINT: u32 = 0x13261f;
    pub const WARNING_TINT: u32 = 0x2a2214;
    pub const DANGER_TINT: u32 = 0x2e181b;
    pub const INFO_TINT: u32 = 0x14232f;
    pub const DANGER_SOLID: u32 = 0xc42b1c;
    pub const ON_DANGER: u32 = 0xffffff;
    pub const TRACK_OFF: u32 = 0x3b3c47;
    pub const TRACK_OFF_HOVER: u32 = 0x464855;
    pub const TRACK_ON: u32 = 0x5a69e0;
    pub const TRACK_ON_HOVER: u32 = 0x6170e4;
    pub const KNOB: u32 = 0xf4f5f8;
    pub const FOCUS_RING: u32 = 0xadb8ff;
    pub const DISABLED_SURFACE: u32 = 0x17181d;
    pub const DISABLED_TEXT: u32 = 0x7b7f89;
    pub const LOADING_TRACK: u32 = 0x262830;
    pub const LOADING_FILL: u32 = 0x97a5ff;
    pub const SCRIM: u32 = 0x040508;
    pub const SHADOW: u32 = 0x000000;
    pub const BRAND_END: u32 = 0xb29bfc;
    pub const AVATARS: [(u32, u32); 6] = [
        (ACCENT_TINT, ACCENT_ON_TINT),
        (0x1a2430, 0xbcd2e6),
        (0x16262a, 0xa9d0cb),
        (0x282219, 0xe0cfa4),
        (0x1e2620, 0xb3cfb0),
        (0x2a1f1d, 0xe0bcae),
    ];
    pub const TAB_ACCENTS: [u32; 6] = [0x9cbcd8, 0xb9aecb, 0x8fc6cf, 0xaec79a, 0xd3bc9c, 0xd4a8ae];
}

/// The light palette: cool paper.
///
/// Content is white on a faintly grey floor, and the chrome is one step darker than the
/// floor. In light the layers above content stay white and are told apart by their
/// shadow, which is what a light window does well.
#[allow(dead_code)]
pub mod light {
    pub const CANVAS: u32 = 0xf3f4f7;
    pub const CHROME: u32 = 0xebecf0;
    pub const SURFACE: u32 = 0xffffff;
    pub const RAISED: u32 = 0xffffff;
    pub const OVERLAY: u32 = 0xffffff;
    pub const SURFACE_HOVER: u32 = 0xeef0f4;
    pub const SURFACE_SELECTED: u32 = 0xe5e7ee;
    pub const SURFACE_PRESSED: u32 = 0xdcdfe7;
    pub const BORDER: u32 = 0xe1e3e9;
    pub const BORDER_STRONG: u32 = 0xc9ccd5;
    pub const SEGMENT_TRACK: u32 = 0xe9ebf0;
    pub const SEGMENT_THUMB: u32 = 0xffffff;
    pub const TEXT_PRIMARY: u32 = 0x16171d;
    pub const TEXT_SECONDARY: u32 = 0x444652;
    pub const TEXT_MUTED: u32 = 0x5a5d68;
    pub const ACCENT: u32 = 0x4053cc;
    pub const ACCENT_HOVER: u32 = 0x3646b4;
    pub const ACCENT_PRESSED: u32 = 0x2e3c9b;
    pub const ON_ACCENT: u32 = 0xffffff;
    pub const ACCENT_TINT: u32 = 0xeceffe;
    pub const ACCENT_ON_TINT: u32 = 0x3442a6;
    pub const POSITIVE: u32 = 0x0f6e49;
    pub const WARNING: u32 = 0x7a5300;
    pub const DANGER: u32 = 0xb42b3c;
    pub const INFO: u32 = 0x1d5f8f;
    pub const PLANNED: u32 = 0x5b606a;
    pub const POSITIVE_TINT: u32 = 0xdff2e8;
    pub const WARNING_TINT: u32 = 0xfaf0d8;
    pub const DANGER_TINT: u32 = 0xfce7e9;
    pub const INFO_TINT: u32 = 0xe2eff9;
    pub const DANGER_SOLID: u32 = 0xc42b1c;
    pub const ON_DANGER: u32 = 0xffffff;
    pub const TRACK_OFF: u32 = 0x878b96;
    pub const TRACK_OFF_HOVER: u32 = 0x7a7e89;
    pub const TRACK_ON: u32 = 0x4053cc;
    pub const TRACK_ON_HOVER: u32 = 0x3646b4;
    pub const KNOB: u32 = 0xffffff;
    pub const FOCUS_RING: u32 = 0x3646b4;
    pub const DISABLED_SURFACE: u32 = 0xf0f1f4;
    pub const DISABLED_TEXT: u32 = 0x767b84;
    pub const LOADING_TRACK: u32 = 0xe2e4e9;
    pub const LOADING_FILL: u32 = 0x4053cc;
    pub const SCRIM: u32 = 0x1a1c22;
    pub const SHADOW: u32 = 0x101828;
    pub const BRAND_END: u32 = 0x7c5cd6;
    pub const AVATARS: [(u32, u32); 6] = [
        (ACCENT_TINT, ACCENT_ON_TINT),
        (0xe4eef7, 0x1c4a6b),
        (0xdff1ee, 0x14574f),
        (0xf8efd9, 0x6b4a00),
        (0xe6f1e4, 0x2d5a2a),
        (0xf9e9e3, 0x7a3a24),
    ];
    pub const TAB_ACCENTS: [u32; 6] = [0x2f5f86, 0x5d4a80, 0x1a6b74, 0x51662f, 0x8a5a20, 0x8b4450];

    /// The terminal's default colours and its 16 ANSI colours, light.
    ///
    /// Keyed by the dark values of `shellune_terminal::palette`; a colour a program
    /// set itself (256-colour or true colour) is drawn as it asked.
    pub const TERMINAL: &[(u32, u32)] = &[
        (0xe6e9f0, 0x1f2329), // foreground
        (0x0b0c10, 0xfcfcfd), // background
        (0x8b9ffd, 0x3f52c8), // cursor
        (0x2b313d, 0x3a3e48), // black
        (0xff8d99, 0xc8334a), // red
        (0x64d8a8, 0x2a7a3f), // green
        (0xf3c66c, 0x8a5c00), // yellow
        (0x9db4ff, 0x3157c9), // blue
        (0xd7a0ff, 0x8c35a8), // magenta
        (0x7fd8e8, 0x0b7286), // cyan
        (0xb5bdcc, 0x80879a), // white
        (0x828b9c, 0x646b7b), // bright black
        (0xffb3bc, 0xb52a3f), // bright red
        (0x8de6c0, 0x246c37), // bright green
        (0xf8d894, 0x7a5100), // bright yellow
        (0xbccbff, 0x2a4cb6), // bright blue
        (0xe3c4ff, 0x7b2b95), // bright magenta
        (0xa8e9f4, 0x0a6474), // bright cyan
        (0xeef1f7, 0x4a4f5c), // bright white
    ];
}

/// The colour tokens views paint with.
///
/// The interface takes its colours from the logo: a blue-black graphite in the dark and
/// a cool paper grey in the light, with exactly one chromatic identity colour - the
/// logo's periwinkle - as the accent for the current object and the one primary action.
/// Nothing else in the regular interface is saturated, so a busy session list stays
/// quiet and the only colours that shout are the status tones.
pub mod palette {
    use super::{Token, dark, light};

    macro_rules! tokens {
        ($($(#[$doc:meta])* $name:ident;)*) => {
            $($(#[$doc])* pub const $name: Token = Token::new(dark::$name, light::$name);)*
        };
    }

    tokens! {
        /// Layer 0, the window's floor: the content area behind cards, and the frame
        /// around the terminal.
        CANVAS;
        /// Layer 1, the chrome: the title bar, the tab strip, the rail and the status
        /// bar.
        CHROME;
        /// Layer 2, content: cards, list rows, inputs and the current tab, which joins
        /// the page under it.
        SURFACE;
        /// Layer 3, things that float over content: menus, popovers, tooltips, toasts.
        RAISED;
        /// Layer 4, dialogs: the front-most surface, drawn over the scrim.
        OVERLAY;
        /// A surface under the pointer.
        SURFACE_HOVER;
        /// The selected navigation entry, tab or choice.
        SURFACE_SELECTED;
        /// A control under the pressed pointer, one step past selected, so a press is
        /// felt and not only tinted.
        SURFACE_PRESSED;
        /// The weak hairline between regions and around cards.
        BORDER;
        /// The strong edge of controls, floating layers and an element under the pointer.
        BORDER_STRONG;
        /// The sunken track a segmented control's options sit in.
        SEGMENT_TRACK;
        /// The raised thumb under the chosen option of a segmented control.
        SEGMENT_THUMB;
        /// Primary text: titles and the current object.
        TEXT_PRIMARY;
        /// Secondary text: body copy and list metadata.
        TEXT_SECONDARY;
        /// Muted text: hints, counts and the status bar. It keeps the body floor on the
        /// pressed surface too, not just on the resting ones.
        TEXT_MUTED;
        /// The selected destination and the primary action.
        ACCENT;
        /// A solid primary button under the pointer.
        ACCENT_HOVER;
        /// A pressed primary button: one step down, so a press sinks.
        ACCENT_PRESSED;
        /// Text on a solid [`ACCENT`] fill.
        ON_ACCENT;
        /// A soft accent fill.
        ACCENT_TINT;
        /// Text and icons on [`ACCENT_TINT`].
        ACCENT_ON_TINT;
        /// Live and completed status; also its text on [`POSITIVE_TINT`].
        POSITIVE;
        /// Waiting, verifying or in-progress status.
        WARNING;
        /// Failed, rejected or unavailable status.
        DANGER;
        /// Neutral information that still wants attention.
        #[cfg_attr(not(test), allow(dead_code))]
        INFO;
        /// Planned-but-not-shipped status; deliberately desaturated.
        PLANNED;
        /// The tint behind a positive chip or banner.
        POSITIVE_TINT;
        /// The tint behind a warning chip or banner.
        WARNING_TINT;
        /// The tint behind a danger chip, banner or destructive button.
        DANGER_TINT;
        /// The tint behind an information banner.
        #[cfg_attr(not(test), allow(dead_code))]
        INFO_TINT;
        /// A solid destructive fill, such as the window's close button under the
        /// pointer.
        DANGER_SOLID;
        /// Text and icons on [`DANGER_SOLID`].
        ON_DANGER;
        /// The track of a switch that is off. The knob keeps the graphic floor on it.
        #[cfg_attr(not(test), allow(dead_code))]
        TRACK_OFF;
        /// [`TRACK_OFF`] under the pointer.
        #[cfg_attr(not(test), allow(dead_code))]
        TRACK_OFF_HOVER;
        /// The track of a switch that is on: the accent's hue, deep enough in the dark
        /// that the light knob keeps the graphic floor on it.
        #[cfg_attr(not(test), allow(dead_code))]
        TRACK_ON;
        /// [`TRACK_ON`] under the pointer.
        #[cfg_attr(not(test), allow(dead_code))]
        TRACK_ON_HOVER;
        /// The knob of a switch, in either state.
        #[cfg_attr(not(test), allow(dead_code))]
        KNOB;
        /// The ring around the control the keyboard is on. Always drawn together with a
        /// thicker edge, never instead of one.
        FOCUS_RING;
        /// A control that cannot be used right now.
        DISABLED_SURFACE;
        /// Text on [`DISABLED_SURFACE`]; it keeps the graphic floor.
        DISABLED_TEXT;
        /// The groove an in-flight indicator travels in.
        LOADING_TRACK;
        /// The in-flight indicator itself.
        LOADING_FILL;
        /// What a dialog lays over the page beneath it, at [`super::SCRIM_ALPHA`].
        SCRIM;
        /// The ink of an elevation shadow.
        SHADOW;
        /// The deeper stop of the one accent gradient, the logo's lavender tail.
        BRAND_END;
    }

    /// Host avatar pairs (tint, initials): accent, slate, steel, sand, sage, clay.
    ///
    /// The first pair is the accent tint, kept for the first host group. The rest are
    /// low-chroma earth and metal tints: an avatar says which host this is, so it must
    /// not compete with the accent that says which host is current.
    pub const AVATARS: [(Token, Token); 6] = {
        let mut pairs = [(CANVAS, CANVAS); 6];
        let mut index = 0;
        while index < 6 {
            pairs[index] = (
                Token::new(dark::AVATARS[index].0, light::AVATARS[index].0),
                Token::new(dark::AVATARS[index].1, light::AVATARS[index].1),
            );
            index += 1;
        }
        pairs
    };

    /// Tab identity colours, in `TabAccent::ALL` order: slate, heather, steel, sage,
    /// sand, dust.
    ///
    /// They are deliberately not the status hues: green, amber and red keep meaning
    /// "connected", "waiting" and "failed", and a tab colour must never be read as one
    /// of those.
    pub const TAB_ACCENTS: [Token; 6] = {
        let mut accents = [CANVAS; 6];
        let mut index = 0;
        while index < 6 {
            accents[index] = Token::new(dark::TAB_ACCENTS[index], light::TAB_ACCENTS[index]);
            index += 1;
        }
        accents
    };
}

/// The token a tab's distinguishing colour is drawn with.
///
/// The colour is an identity aid, never a status: a tab keeps its status dot, its
/// state word and its target whatever colour the user gives it.
#[must_use]
pub fn tab_accent_token(accent: TabAccent) -> Token {
    let index = TabAccent::ALL
        .iter()
        .position(|candidate| *candidate == accent)
        .unwrap_or(0);
    palette::TAB_ACCENTS[index]
}

/// The colour a tab's distinguishing mark is drawn in, in the current palette.
#[must_use]
pub fn tab_accent_color(accent: TabAccent) -> Rgba {
    swatch(tab_accent_token(accent))
}

/// What the pointer is doing to a control, for the states that answer it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwatchState {
    /// Nothing points at the control.
    Resting,
    /// The pointer is on it.
    Hovered,
    /// It is being pressed.
    Pressed,
}

/// The two surfaces one colour swatch draws, as palette tokens.
///
/// The values are tokens rather than resolved colours, so the same face is checked
/// in both palettes without a window in the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwatchFace {
    /// The fill behind the colour, when this state paints one.
    pub fill: Option<Token>,
    /// The ring around the colour, when this state draws one.
    pub ring: Option<Token>,
}

impl SwatchFace {
    /// The fill in the palette being drawn.
    #[must_use]
    pub fn fill_color(self) -> Rgba {
        self.fill.map_or_else(clear, swatch)
    }

    /// The ring in the palette being drawn.
    #[must_use]
    pub fn ring_color(self) -> Rgba {
        self.ring.map_or_else(clear, swatch)
    }
}

/// The face of a tab-colour swatch.
///
/// A swatch answers the pointer the way the buttons beside it do: a `SURFACE_HOVER`
/// fill with a `BORDER_STRONG` ring under the pointer, and one step further to
/// `SURFACE_SELECTED` with the accent ring while it is pressed. The chosen colour
/// keeps its `TEXT_PRIMARY` ring in every state, so the row still says which colour
/// is set while the pointer rests on another one. Neither value is a status: the ring
/// marks a choice and the fill marks the pointer, never the session.
#[must_use]
pub fn accent_swatch_face(chosen: bool, state: SwatchState) -> SwatchFace {
    let ring_when_chosen = chosen.then_some(palette::TEXT_PRIMARY);
    match state {
        SwatchState::Resting => SwatchFace {
            fill: None,
            ring: ring_when_chosen,
        },
        SwatchState::Hovered => SwatchFace {
            fill: Some(palette::SURFACE_HOVER),
            ring: Some(ring_when_chosen.unwrap_or(palette::BORDER_STRONG)),
        },
        SwatchState::Pressed => SwatchFace {
            fill: Some(palette::SURFACE_SELECTED),
            ring: Some(ring_when_chosen.unwrap_or(palette::ACCENT)),
        },
    }
}

/// Spacing scale in logical pixels before zoom is applied.
pub mod spacing {
    /// 4px — icon to label.
    pub const HAIRLINE: f32 = 4.0;
    /// 8px — inside a chip.
    pub const TIGHT: f32 = 8.0;
    /// 12px — inside a row.
    pub const SNUG: f32 = 12.0;
    /// 16px — panel padding.
    pub const BASE: f32 = 16.0;
    /// 24px — between panels.
    pub const LOOSE: f32 = 24.0;
}

/// Corner radii in logical pixels: 6, 8, 10, 14 and 20.
///
/// The radius grows with the object: the smaller a thing is, the tighter its corner, so
/// a chip inside a card does not look rounder than the card.
pub mod radius {
    /// Chips, keycaps, checkboxes and small controls.
    pub const SMALL: f32 = 6.0;
    /// Buttons, rows, menu entries and inputs.
    pub const MEDIUM: f32 = 8.0;
    /// Cards, menus, toasts, tabs and the terminal frame.
    pub const LARGE: f32 = 10.0;
    /// Dialogs and hero tiles.
    pub const XLARGE: f32 = 14.0;
    /// The welcome emblem and the splash mark.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const XXLARGE: f32 = 20.0;
}

/// Type scale in logical pixels.
pub mod type_scale {
    /// Status bar and shortcut hints.
    pub const CAPTION: f32 = 11.0;
    /// Labels, chips and list metadata.
    pub const LABEL: f32 = 12.0;
    /// Body copy and list rows.
    pub const BODY: f32 = 13.0;
    /// Panel title.
    pub const TITLE: f32 = 15.0;
    /// Destination title in the header and dialog titles.
    pub const HEADING: f32 = 17.0;
    /// A page title.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const DISPLAY: f32 = 22.0;
    /// The welcome title on an empty page.
    pub const HERO: f32 = 28.0;
}

/// The three weights the interface uses.
///
/// Regular for reading, medium for labels and controls, semibold for titles. Bold is not
/// used: at interface sizes it smears more than it stresses.
pub mod weight {
    use gpui::FontWeight;

    /// Body copy.
    pub const NORMAL: FontWeight = FontWeight::NORMAL;
    /// Button labels, chips, list names and the current tab.
    pub const MEDIUM: FontWeight = FontWeight::MEDIUM;
    /// Titles and headings.
    pub const SEMIBOLD: FontWeight = FontWeight::SEMIBOLD;
}

/// Resolves a token in the palette the window is being drawn in.
///
/// Views never call `rgb` with a literal; this is the one place a token turns into a
/// colour.
pub fn swatch(token: Token) -> Rgba {
    rgb(resolve(token))
}

/// Which palette the window is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    /// The dark palette: the tokens' own values.
    #[default]
    Dark,
    /// The light palette: the tokens' light values, chosen for a light window.
    Light,
}

/// What the user chose for the palette.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Follow the operating system's light or dark setting.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// Whether short interface motion is played.
///
/// The user's preference is one input; the operating system's "reduce motion"
/// setting is the other. GPUI 0.2.2 does not report the system setting, so the shell
/// reads it itself ([`system_reduces_motion`]) and draws with
/// [`Motion::with_system`]: motion is reduced when either one asks for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Play short, skippable feedback such as a loading dot fading in.
    #[default]
    Full,
    /// Skip that motion. Status text, colour and available actions stay.
    Reduced,
}

impl Motion {
    /// A small state change: a dot fading in, a chip appearing. 120 ms.
    pub const FAST: Duration = Duration::from_millis(120);
    /// An object entering: a menu, a toast, a card. 180 ms.
    pub const BASE: Duration = Duration::from_millis(180);
    /// A large surface entering: a dialog, the splash. 260 ms.
    pub const SLOW: Duration = Duration::from_millis(260);
    /// One beat of an in-flight indicator that keeps pulsing while work runs. 900 ms.
    pub const PULSE: Duration = Duration::from_millis(900);

    /// The motion a window draws with, given whether the system asks for less.
    ///
    /// Reduced wins: the system setting can only take motion away, never add it back
    /// against the user's own choice.
    #[must_use]
    pub fn with_system(self, system_reduces: bool) -> Self {
        if system_reduces { Self::Reduced } else { self }
    }

    /// Whether a short animation may run.
    #[must_use]
    pub fn allows_animation(self) -> bool {
        matches!(self, Self::Full)
    }

    /// The other setting.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::Reduced,
            Self::Reduced => Self::Full,
        }
    }
}

impl ThemeChoice {
    /// The order the theme control steps through.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// The next choice in [`ThemeChoice::ALL`].
    #[must_use]
    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    /// The palette to draw in, given whether the system is in dark mode.
    #[must_use]
    pub fn appearance(self, system_dark: bool) -> Appearance {
        match self {
            Self::System if system_dark => Appearance::Dark,
            Self::System | Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
        }
    }
}

/// The terminal's own palette, which may differ from the window's: many programs
/// are written for a dark terminal, and some people want one in a light window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalScheme {
    /// The same palette as the window.
    #[default]
    FollowApp,
    /// Always the dark terminal palette.
    Dark,
    /// Always the light terminal palette.
    Light,
}

impl TerminalScheme {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 3] = [Self::FollowApp, Self::Dark, Self::Light];

    /// The palette the terminal is drawn in, given the window's.
    #[must_use]
    pub fn appearance(self, window: Appearance) -> Appearance {
        match self {
            Self::FollowApp => window,
            Self::Dark => Appearance::Dark,
            Self::Light => Appearance::Light,
        }
    }
}

thread_local! {
    /// The palette the window is being drawn in; per thread, so tests that each run a
    /// window on their own thread never see another test's choice.
    static APPEARANCE: std::cell::Cell<Appearance> = const { std::cell::Cell::new(Appearance::Dark) };
    /// The palette the terminal grid is drawn in.
    static TERMINAL_APPEARANCE: std::cell::Cell<Appearance> = const { std::cell::Cell::new(Appearance::Dark) };
    /// The motion of the window being drawn: the user's preference combined with the
    /// system's reduce-motion setting.
    static MOTION: std::cell::Cell<Motion> = const { std::cell::Cell::new(Motion::Full) };
    /// The text size of the window being drawn.
    static ZOOM: std::cell::Cell<Zoom> = const { std::cell::Cell::new(Zoom::Regular) };
}

/// The palette the window is drawn in now.
#[must_use]
pub fn appearance() -> Appearance {
    APPEARANCE.with(std::cell::Cell::get)
}

/// Sets the palette the next draws use; the window calls it before drawing.
pub fn set_appearance(appearance: Appearance) {
    APPEARANCE.with(|current| current.set(appearance));
}

/// The palette the terminal grid is drawn in now.
#[must_use]
pub fn terminal_appearance() -> Appearance {
    TERMINAL_APPEARANCE.with(std::cell::Cell::get)
}

/// Sets the palette the terminal grid is drawn in; the window calls it before
/// drawing.
pub fn set_terminal_appearance(appearance: Appearance) {
    TERMINAL_APPEARANCE.with(|current| current.set(appearance));
}

/// Whether the window being drawn may play short motion.
#[must_use]
pub fn motion_allowed() -> bool {
    MOTION.with(|current| current.get().allows_animation())
}

/// Sets the motion the next draws use; the window passes its preference already
/// combined with the system setting ([`Motion::with_system`]).
pub fn set_motion(motion: Motion) {
    MOTION.with(|current| current.set(motion));
}

/// Whether reading the system's reduce-motion setting may block for a while.
///
/// On Linux the setting lives in the desktop's settings store, which is read through
/// a helper program; the window asks off the main thread there. Windows and macOS
/// answer from memory, so the window asks inline and its first frame is already right.
pub const SYSTEM_MOTION_QUERY_BLOCKS: bool = cfg!(all(unix, not(target_os = "macos"), not(test)));

/// Whether the operating system asks applications to reduce motion.
///
/// * Windows: "Animation effects" is off (`SPI_GETCLIENTAREAANIMATION`).
/// * macOS: Accessibility > Display > Reduce motion
///   (`NSWorkspace.accessibilityDisplayShouldReduceMotion`).
/// * Linux and other Unix desktops: GNOME's `enable-animations` is false, or KDE's
///   animation speed is set to instant (`AnimationDurationFactor=0`).
///
/// A setting that cannot be read counts as "not asked": the user's own preference
/// still applies. Tests read a per-thread stand-in instead of the machine they run on,
/// so a developer's system setting never changes what a test sees.
#[must_use]
pub fn system_reduces_motion() -> bool {
    #[cfg(test)]
    {
        FAKE_SYSTEM_REDUCES_MOTION.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        system_motion::reduces_motion()
    }
}

#[cfg(test)]
thread_local! {
    /// What [`system_reduces_motion`] answers in a test on this thread.
    static FAKE_SYSTEM_REDUCES_MOTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes [`system_reduces_motion`] answer `reduces` on this test thread.
#[cfg(test)]
pub fn fake_system_reduces_motion(reduces: bool) {
    FAKE_SYSTEM_REDUCES_MOTION.with(|current| current.set(reduces));
}

/// The platform reads behind [`system_reduces_motion`].
#[cfg(not(test))]
mod system_motion {
    #[cfg(windows)]
    pub fn reduces_motion() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut animate: windows_sys::core::BOOL = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL through the pointer, which
        // points at a live BOOL for the duration of the call.
        let read = unsafe {
            SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animate).cast(), 0)
        };
        read != 0 && animate == 0
    }

    #[cfg(target_os = "macos")]
    pub fn reduces_motion() -> bool {
        use std::ffi::{c_char, c_void};

        #[link(name = "AppKit", kind = "framework")]
        unsafe extern "C" {}
        #[link(name = "objc")]
        unsafe extern "C" {
            fn objc_getClass(name: *const c_char) -> *mut c_void;
            fn sel_registerName(name: *const c_char) -> *mut c_void;
            fn objc_msgSend();
        }

        type SendObject = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
        type SendBool = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u8;
        // SAFETY: the messages are sent with the signatures AppKit declares:
        // +[NSWorkspace sharedWorkspace] returns an object and
        // -accessibilityDisplayShouldReduceMotion returns a BOOL (one byte). Both are
        // plain getters that do not autorelease, and a missing class or a nil
        // workspace is checked before the next send.
        unsafe {
            let class = objc_getClass(c"NSWorkspace".as_ptr());
            if class.is_null() {
                return false;
            }
            let send = objc_msgSend as unsafe extern "C" fn();
            let send_object: SendObject = std::mem::transmute(send);
            let send_bool: SendBool = std::mem::transmute(send);
            let workspace = send_object(class, sel_registerName(c"sharedWorkspace".as_ptr()));
            if workspace.is_null() {
                return false;
            }
            send_bool(
                workspace,
                sel_registerName(c"accessibilityDisplayShouldReduceMotion".as_ptr()),
            ) != 0
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn reduces_motion() -> bool {
        gnome_animations_off() || kde_animations_instant()
    }

    /// GNOME, and the desktops that share its settings, turn animations off here.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn gnome_animations_off() -> bool {
        std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "enable-animations"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| {
                output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "false"
            })
    }

    /// KDE's "animation speed: instant" is a zero duration factor in kdeglobals.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn kde_animations_instant() -> bool {
        let Some(config) = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
            })
        else {
            return false;
        };
        let Ok(text) = std::fs::read_to_string(config.join("kdeglobals")) else {
            return false;
        };
        let mut in_kde = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_kde = line == "[KDE]";
            } else if in_kde
                && let Some((key, value)) = line.split_once('=')
                && key.trim() == "AnimationDurationFactor"
            {
                return value
                    .trim()
                    .parse::<f64>()
                    .is_ok_and(|factor| factor <= 0.0);
            }
        }
        false
    }

    #[cfg(not(any(windows, unix)))]
    pub fn reduces_motion() -> bool {
        false
    }
}

/// The metrics of the window being drawn, for the pieces drawn outside the window's own
/// render pass - the terminal's frame and a tooltip - which would otherwise fall back to
/// 100% and ignore the user's text size.
#[must_use]
pub fn metrics() -> Metrics {
    Metrics::new(ZOOM.with(std::cell::Cell::get))
}

/// Sets the zoom the next draws use; the window calls it before drawing.
pub fn set_zoom(zoom: Zoom) {
    ZOOM.with(|current| current.set(zoom));
}

/// How an object arrives on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enter {
    /// Fades in where it is: toasts, tooltips, a chip.
    Fade,
    /// Fades in while rising a few pixels into place: cards, menus.
    #[cfg_attr(not(test), allow(dead_code))]
    Rise,
    /// Fades in while settling from slightly above: dialogs and the splash. GPUI
    /// cannot scale a box, so the settle stands in for the slight zoom.
    #[cfg_attr(not(test), allow(dead_code))]
    Settle,
}

impl Enter {
    /// How long this entrance takes.
    #[must_use]
    pub fn duration(self) -> Duration {
        match self {
            Self::Fade => Motion::FAST,
            Self::Rise => Motion::BASE,
            Self::Settle => Motion::SLOW,
        }
    }

    /// How far the object travels, in logical pixels; negative is from above.
    fn travel(self) -> f32 {
        match self {
            Self::Fade => 0.0,
            Self::Rise => 6.0,
            Self::Settle => -8.0,
        }
    }
}

/// The curve every entrance follows: quick out of the gate, settling softly.
pub fn ease_out() -> impl Fn(f32) -> f32 {
    gpui::ease_out_quint()
}

/// Plays an entrance on `element` when motion is allowed.
///
/// Motion is only ever an entrance: GPUI has no hover transitions, so the pointer
/// changes a face at once, and nothing loops except an in-flight indicator. With motion
/// reduced the element is drawn in its final place at once. The `id` keys the
/// animation, so a re-render with the same id does not play it again.
///
/// The travel never changes how the element is positioned: an element in the flow is
/// offset relatively (its siblings do not move), and an absolutely placed one - a menu
/// anchored to a button, a toast stack pinned to a corner - is nudged by a margin on the
/// edge it is anchored to, so it stays out of the flow and in its own place.
pub fn enter<E>(element: E, id: impl Into<ElementId>, kind: Enter) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    if !motion_allowed() {
        return element.into_any_element();
    }
    let travel = kind.travel();
    element
        .with_animation(
            id,
            Animation::new(kind.duration()).with_easing(ease_out()),
            move |element, delta| {
                let mut element = element.opacity(delta);
                if travel != 0.0 {
                    travel_by(element.style(), px(travel * (1.0 - delta)));
                }
                element
            },
        )
        .into_any_element()
}

/// Moves a styled element `offset` down (negative: up) without changing its position
/// type. See [`enter`].
fn travel_by(style: &mut gpui::StyleRefinement, offset: Pixels) {
    if style.position == Some(gpui::Position::Absolute) {
        let from_bottom = style.inset.top.is_none() && style.inset.bottom.is_some();
        if from_bottom {
            style.margin.bottom = Some(nudged(style.margin.bottom, -offset));
        } else {
            style.margin.top = Some(nudged(style.margin.top, offset));
        }
    } else {
        style.position = Some(gpui::Position::Relative);
        style.inset.top = Some(nudged(style.inset.top, offset));
    }
}

/// `existing` moved by `offset` when it is a pixel length; otherwise just the offset.
fn nudged(existing: Option<gpui::Length>, offset: Pixels) -> gpui::Length {
    use gpui::{AbsoluteLength, DefiniteLength, Length};
    let base = match existing {
        Some(Length::Definite(DefiniteLength::Absolute(AbsoluteLength::Pixels(base)))) => base,
        _ => px(0.0),
    };
    Length::Definite(DefiniteLength::Absolute(AbsoluteLength::Pixels(
        base + offset,
    )))
}

/// How far an object floats above what it sits on.
///
/// Each level is a pair of shadows - a tight contact shadow and a soft ambient one - so
/// a raised object has an edge and a presence. Dark shadows are deeper: on a dark floor
/// a light shadow does not read at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevation {
    /// On the page: no shadow. Rows, the chrome, inline controls.
    #[cfg_attr(not(test), allow(dead_code))]
    Flat,
    /// Resting on the page: cards, inputs, the chosen segment.
    Low,
    /// Over the page: menus, popovers, tooltips and toasts.
    Mid,
    /// Over everything: dialogs and the splash.
    High,
}

impl Elevation {
    /// The shadows of this level in the palette being drawn.
    #[must_use]
    pub fn shadows(self) -> Vec<BoxShadow> {
        let dark = appearance() == Appearance::Dark;
        // (y offset, blur, spread, alpha) for the contact and the ambient shadow.
        let layers: &[(f32, f32, f32, f32)] = match (self, dark) {
            (Self::Flat, _) => &[],
            (Self::Low, false) => &[(1.0, 2.0, 0.0, 0.06), (1.0, 3.0, 0.0, 0.08)],
            (Self::Low, true) => &[(1.0, 2.0, 0.0, 0.32), (1.0, 4.0, 0.0, 0.24)],
            (Self::Mid, false) => &[(2.0, 4.0, -1.0, 0.06), (8.0, 20.0, -4.0, 0.14)],
            (Self::Mid, true) => &[(2.0, 4.0, -1.0, 0.40), (10.0, 24.0, -4.0, 0.50)],
            (Self::High, false) => &[(4.0, 8.0, -2.0, 0.08), (20.0, 48.0, -8.0, 0.22)],
            (Self::High, true) => &[(4.0, 8.0, -2.0, 0.45), (24.0, 56.0, -8.0, 0.65)],
        };
        let ink = swatch(palette::SHADOW);
        layers
            .iter()
            .map(|&(y, blur, spread, alpha)| BoxShadow {
                color: faded(ink, alpha).into(),
                offset: point(px(0.0), px(y)),
                blur_radius: px(blur),
                spread_radius: px(spread),
            })
            .collect()
    }
}

/// Lets any styled element take an [`Elevation`] instead of a GPUI shadow preset.
pub trait Elevated: Styled + Sized {
    /// Draws this element at `level`.
    #[must_use]
    fn elevation(self, level: Elevation) -> Self {
        let shadows = level.shadows();
        if shadows.is_empty() {
            self
        } else {
            self.shadow(shadows)
        }
    }
}

impl<T: Styled + Sized> Elevated for T {}

/// The value a token has in the palette the window is being drawn in.
#[must_use]
pub fn resolve(token: Token) -> u32 {
    token.value(appearance())
}

/// A terminal colour in the current palette.
#[must_use]
pub fn terminal_color(color: u32) -> u32 {
    match terminal_appearance() {
        Appearance::Dark => color,
        Appearance::Light => light::TERMINAL
            .iter()
            .find(|(dark, _)| *dark == color)
            .map_or(color, |(_, light)| *light),
    }
}

/// A colour at a reduced opacity, for borders and hairlines that should recede.
pub fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// A fully transparent colour, for a border or fill that only appears in some states.
///
/// Keeping the border in place, invisible, means an element does not shift by a
/// pixel when it becomes selected or focused.
pub fn clear() -> Rgba {
    gpui::rgba(0x0000_0000)
}

/// How much of the page a dialog hides behind itself.
///
/// The page stays legible underneath: the backdrop only has to make the dialog the
/// front-most object, not to black the window out.
pub const SCRIM_ALPHA: f32 = 0.62;

/// The backdrop a dialog draws over the page beneath it.
///
/// It is a translucent [`palette::SCRIM`] rather than a solid fill, so a dialog does
/// not look like a second window and the page keeps its place.
#[must_use]
pub fn scrim() -> Rgba {
    faded(swatch(palette::SCRIM), SCRIM_ALPHA)
}

/// Semantic colour role attached to a state label, chip or dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Nothing is running; neutral information.
    Neutral,
    /// The current object or a primary action.
    Accent,
    /// Live, connected or completed.
    Positive,
    /// Waiting, connecting or a recoverable warning.
    Warning,
    /// Failed, rejected, lost or unavailable.
    Danger,
    /// A capability this build does not ship yet.
    Planned,
}

/// The silhouette a status mark is drawn with.
///
/// A state is never told by colour alone (R8): the mark differs in shape, the state
/// word names it, and only then does the tone colour agree with both. The shapes stay
/// inside one small square so a chip's geometry does not change with its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusMark {
    /// A solid disc: live, connected, completed.
    Disc,
    /// A hollow ring: waiting, verifying, connecting.
    Ring,
    /// A solid square: failed, rejected, unavailable.
    Square,
    /// A hollow square: a capability this build does not ship.
    Box,
    /// A vertical bar: the current object.
    Bar,
    /// A horizontal dash: nothing is running here.
    Dash,
}

impl Tone {
    /// The silhouette this tone is drawn with wherever a mark stands beside a word.
    ///
    /// The mapping is fixed per tone so the same state looks the same in a list row, a
    /// chip, a banner and the status bar.
    #[must_use]
    pub fn mark(self) -> StatusMark {
        match self {
            Self::Positive => StatusMark::Disc,
            Self::Warning => StatusMark::Ring,
            Self::Danger => StatusMark::Square,
            Self::Planned => StatusMark::Box,
            Self::Accent => StatusMark::Bar,
            Self::Neutral => StatusMark::Dash,
        }
    }

    /// Text colour for a label in this tone.
    pub fn text(self) -> Rgba {
        swatch(self.token())
    }

    /// The token behind [`Tone::text`], exposed for contrast tests.
    pub fn token(self) -> Token {
        match self {
            Self::Neutral => palette::TEXT_SECONDARY,
            Self::Accent => palette::ACCENT,
            Self::Positive => palette::POSITIVE,
            Self::Warning => palette::WARNING,
            Self::Danger => palette::DANGER,
            Self::Planned => palette::PLANNED,
        }
    }

    /// Surface colour behind a chip or banner in this tone.
    ///
    /// Each status tone gets its own dark tint, so a chip reads by colour before it
    /// is read by word; the tints stay dark enough that the tone's text keeps its
    /// contrast on them.
    pub fn chip_background(self) -> Rgba {
        swatch(self.tint())
    }

    /// The token behind [`Tone::chip_background`].
    pub fn tint(self) -> Token {
        match self {
            Self::Accent => palette::ACCENT_TINT,
            Self::Positive => palette::POSITIVE_TINT,
            Self::Warning => palette::WARNING_TINT,
            Self::Danger => palette::DANGER_TINT,
            Self::Neutral | Self::Planned => palette::SURFACE_SELECTED,
        }
    }

    /// Maps a session lifecycle state to the only tone it may be drawn in.
    ///
    /// The mapping is exhaustive on purpose: only a state that claims a live
    /// terminal gets the positive tone, and every failure keeps the danger tone
    /// while the session is still listed.
    pub fn for_session_state(state: &SessionState) -> Self {
        match state {
            SessionState::Idle => Self::Neutral,
            SessionState::Connecting
            | SessionState::VerifyingHost
            | SessionState::Authenticating => Self::Warning,
            SessionState::Connected => Self::Positive,
            // A process that ended with status 0 ended as expected; painting that as
            // an error would make a normal exit look like a failure. Whether it was live
            // first is not the question - the reason it ended is.
            SessionState::Disconnected(failure) | SessionState::Failed(failure)
                if failure.ended_as_expected() =>
            {
                Self::Neutral
            }
            SessionState::Disconnected(_) | SessionState::Failed(_) => Self::Danger,
            SessionState::Closed => Self::Planned,
        }
    }

    /// Maps the reason a session ended to the tone it may be drawn in.
    ///
    /// The same rule as [Self::for_session_state], for the places that hold a reason
    /// without a state: a process that ended with status 0 is stated in the neutral tone
    /// and every other reason is a real failure (R12).
    pub fn for_failure(failure: &SessionFailure) -> Self {
        if failure.ended_as_expected() {
            Self::Neutral
        } else {
            Self::Danger
        }
    }

    /// Maps a transfer state to its tone; only `Completed` is positive.
    pub fn for_transfer_state(state: &TransferState) -> Self {
        match state {
            TransferState::Queued
            | TransferState::AwaitingOverwrite { .. }
            | TransferState::Running { .. } => Self::Warning,
            TransferState::Completed { .. } => Self::Positive,
            TransferState::Failed { .. } | TransferState::Cancelled { .. } => Self::Danger,
        }
    }
}

/// The user's text-size preference, applied on top of the platform scale factor.
///
/// Zoom changes metrics, not content: no label is hidden at a larger zoom, so the
/// shell cannot claim a capability is absent just because it did not fit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zoom {
    /// 90% — denser lists on small displays.
    Compact,
    /// 100% — the default.
    #[default]
    Regular,
    /// 115% — larger text for high-density displays.
    Comfortable,
    /// 200% — text and the metrics that keep it readable, the resize-text ceiling.
    Largest,
}

impl Zoom {
    /// Every zoom level in display order.
    pub const ALL: [Self; 4] = [
        Self::Compact,
        Self::Regular,
        Self::Comfortable,
        Self::Largest,
    ];

    /// Multiplier applied to every logical length.
    pub fn factor(self) -> f32 {
        match self {
            Self::Compact => 0.9,
            Self::Regular => 1.0,
            Self::Comfortable => 1.15,
            Self::Largest => 2.0,
        }
    }

    /// Percent shown in the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "90%",
            Self::Regular => "100%",
            Self::Comfortable => "115%",
            Self::Largest => "200%",
        }
    }

    /// Next step in [`Zoom::ALL`], wrapping around.
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|zoom| *zoom == self).unwrap_or(1);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// Zoom-aware length helpers.
///
/// Views ask for a named metric instead of writing `px(...)` so that zooming and
/// small windows go through one code path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    zoom: Zoom,
}

/// Smallest edge a primary pointer target may have, in logical pixels (R4).
///
/// A target is not allowed to shrink below this when the text size is reduced: the
/// point of the minimum is the pointer, which does not scale with the interface.
pub const HIT_TARGET: f32 = 24.0;

impl Metrics {
    /// Metrics for a zoom level.
    pub fn new(zoom: Zoom) -> Self {
        Self { zoom }
    }

    /// Scales one logical length by the zoom factor.
    pub fn scale(self, base: f32) -> Pixels {
        px(base * self.zoom.factor())
    }

    /// Width of the navigation rail.
    pub fn rail_width(self) -> Pixels {
        self.scale(208.0)
    }

    /// Height of the header that names the current object.
    pub fn header_height(self) -> Pixels {
        self.scale(46.0)
    }

    /// Height of one list row.
    pub fn list_row_height(self) -> Pixels {
        self.scale(52.0)
    }

    /// Edge length of a navigation or inline icon.
    pub fn icon_size(self) -> Pixels {
        self.scale(16.0)
    }

    /// Edge length of a small icon inside a compact control.
    pub fn small_icon_size(self) -> Pixels {
        self.scale(14.0)
    }

    /// Height of a standard button, input or segmented control.
    pub fn control_height(self) -> Pixels {
        self.scale(32.0)
    }

    /// Height of a compact control inside a toolbar.
    pub fn compact_control_height(self) -> Pixels {
        self.scale(26.0)
    }

    /// Edge length of a square icon control.
    ///
    /// It follows the text size upwards but never below [`HIT_TARGET`]: an icon
    /// button is a pointer target before it is a drawing, and 24×24 is the smallest
    /// that stays comfortable to hit (R4).
    pub fn hit_target(self) -> Pixels {
        self.compact_control_height().max(px(HIT_TARGET))
    }

    /// Height of one session tab.
    pub fn tab_height(self) -> Pixels {
        self.scale(30.0)
    }

    /// Body text size.
    pub fn body_size(self) -> Pixels {
        self.scale(type_scale::BODY)
    }

    /// Label text size.
    pub fn label_size(self) -> Pixels {
        self.scale(type_scale::LABEL)
    }

    /// Caption text size.
    pub fn caption_size(self) -> Pixels {
        self.scale(type_scale::CAPTION)
    }

    /// Panel title size.
    pub fn title_size(self) -> Pixels {
        self.scale(type_scale::TITLE)
    }

    /// Destination heading size.
    pub fn heading_size(self) -> Pixels {
        self.scale(type_scale::HEADING)
    }

    /// Page title size.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn display_size(self) -> Pixels {
        self.scale(type_scale::DISPLAY)
    }

    /// Welcome title size.
    pub fn hero_size(self) -> Pixels {
        self.scale(type_scale::HERO)
    }

    /// Weight for body copy.
    pub fn body_weight(self) -> FontWeight {
        weight::NORMAL
    }

    /// Weight for labels, controls and names in a list.
    pub fn medium_weight(self) -> FontWeight {
        weight::MEDIUM
    }

    /// Weight for titles.
    pub fn title_weight(self) -> FontWeight {
        weight::SEMIBOLD
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(Zoom::default())
    }
}

/// Short label for a session kind, used in list rows and confirmations.
pub fn session_kind_label(kind: &SessionKind, shell_fallback: &str) -> String {
    match kind {
        SessionKind::Local { shell } => shell.clone().unwrap_or_else(|| shell_fallback.to_owned()),
        SessionKind::Remote { .. } => "SSH".to_owned(),
        // A desktop or serial tab says its protocol here. None of them can be started
        // by this build yet, so the label never claims a live connection.
        SessionKind::Rdp { .. } => "RDP".to_owned(),
        SessionKind::Vnc { .. } => "VNC".to_owned(),
        SessionKind::Serial { .. } => "Serial".to_owned(),
    }
}

/// Minimum contrast for body text (WCAG AA).
#[cfg(test)]
pub const BODY_CONTRAST_FLOOR: f64 = 4.5;

/// Minimum contrast for large text, status dots and focus rings (WCAG AA).
#[cfg(test)]
pub const GRAPHIC_CONTRAST_FLOOR: f64 = 3.0;

/// WCAG 2.1 contrast arithmetic.
///
/// It lives behind `cfg(test)` because nothing in the application needs to
/// measure contrast at runtime; the design review and the tests do. Keeping the
/// maths next to the palette means a token change is checked in the same place.
#[cfg(test)]
pub mod contrast {
    /// Relative luminance of a 24-bit sRGB colour.
    pub fn relative_luminance(hex: u32) -> f64 {
        let [_, r, g, b] = hex.to_be_bytes();
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    /// Contrast ratio between two colours, from 1.0 to 21.0.
    pub fn ratio(foreground: u32, background: u32) -> f64 {
        let foreground = relative_luminance(foreground);
        let background = relative_luminance(background);
        let (lighter, darker) = if foreground > background {
            (foreground, background)
        } else {
            (background, foreground)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// The contrast of a token pair in each palette, dark first.
    pub fn both(foreground: super::Token, background: super::Token) -> (f64, f64) {
        (
            ratio(foreground.dark(), background.dark()),
            ratio(foreground.light(), background.light()),
        )
    }

    /// The lower of the pair's two contrasts: the floor holds in both palettes only if
    /// this reaches it.
    pub fn worst(foreground: super::Token, background: super::Token) -> f64 {
        let (dark, light) = both(foreground, background);
        dark.min(light)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Appearance, Elevation, Enter, Motion, StatusMark, Token, Tone, contrast, dark,
        light::{self, TERMINAL},
        palette, radius, type_scale,
    };
    use gpui::prelude::*;
    use gpui::px;

    /// Minimum contrast for body text (WCAG AA).
    const BODY: f64 = 4.5;
    /// Minimum contrast for status dots, marks, focus rings and inactive controls.
    const GRAPHIC: f64 = 3.0;

    /// The five layers, floor first.
    const LAYERS: [(&str, Token); 5] = [
        ("canvas", palette::CANVAS),
        ("chrome", palette::CHROME),
        ("surface", palette::SURFACE),
        ("raised", palette::RAISED),
        ("overlay", palette::OVERLAY),
    ];

    /// Every surface a label can be drawn on: the layers and the interaction fills.
    fn surfaces() -> Vec<(&'static str, Token)> {
        let mut surfaces = LAYERS.to_vec();
        surfaces.extend([
            ("hover", palette::SURFACE_HOVER),
            ("selected", palette::SURFACE_SELECTED),
            ("pressed", palette::SURFACE_PRESSED),
        ]);
        surfaces
    }

    fn assert_floor(name: &str, foreground: Token, background: Token, floor: f64) {
        let (dark, light) = contrast::both(foreground, background);
        assert!(dark >= floor, "dark {name} is {dark:.2}:1, below {floor}:1");
        assert!(
            light >= floor,
            "light {name} is {light:.2}:1, below {floor}:1"
        );
    }

    #[test]
    fn body_text_reaches_four_and_a_half_to_one_on_every_surface_in_both_palettes() {
        // Muted is included: hints, counts and the status bar also sit on a pressed
        // row, so "muted" is not an exemption from the body floor. The status tones
        // are words too ("Connected", "Failed"), so they keep the same floor.
        let text = [
            ("primary", palette::TEXT_PRIMARY),
            ("secondary", palette::TEXT_SECONDARY),
            ("muted", palette::TEXT_MUTED),
            ("accent", palette::ACCENT),
            ("positive", palette::POSITIVE),
            ("warning", palette::WARNING),
            ("danger", palette::DANGER),
            ("info", palette::INFO),
            ("planned", palette::PLANNED),
        ];
        for (surface_name, surface) in surfaces() {
            for (text_name, token) in text {
                assert_floor(
                    &format!("{text_name} on {surface_name}"),
                    token,
                    surface,
                    BODY,
                );
            }
            assert_floor(
                &format!("the focus ring on {surface_name}"),
                palette::FOCUS_RING,
                surface,
                GRAPHIC,
            );
        }
    }

    #[test]
    fn every_status_reads_on_its_own_tint_and_every_fill_carries_its_text() {
        for (name, text, fill) in [
            ("positive", palette::POSITIVE, palette::POSITIVE_TINT),
            ("warning", palette::WARNING, palette::WARNING_TINT),
            ("danger", palette::DANGER, palette::DANGER_TINT),
            ("info", palette::INFO, palette::INFO_TINT),
            ("on accent", palette::ON_ACCENT, palette::ACCENT),
            ("on accent hover", palette::ON_ACCENT, palette::ACCENT_HOVER),
            (
                "on accent pressed",
                palette::ON_ACCENT,
                palette::ACCENT_PRESSED,
            ),
            (
                "accent on tint",
                palette::ACCENT_ON_TINT,
                palette::ACCENT_TINT,
            ),
            ("on danger", palette::ON_DANGER, palette::DANGER_SOLID),
        ] {
            assert_floor(name, text, fill, BODY);
        }
        for (tint, text) in palette::AVATARS {
            assert_floor(&format!("avatar {text:x}"), text, tint, BODY);
        }
        // An inactive control is still read, and an indicator is a graphic.
        assert_floor(
            "disabled text",
            palette::DISABLED_TEXT,
            palette::DISABLED_SURFACE,
            GRAPHIC,
        );
        assert_floor(
            "loading indicator",
            palette::LOADING_FILL,
            palette::LOADING_TRACK,
            GRAPHIC,
        );
    }

    #[test]
    fn the_light_palette_is_its_own_table_not_a_copy_of_the_dark_one() {
        for (name, token) in surfaces() {
            assert_ne!(
                token.dark(),
                token.light(),
                "{name} draws one value in both palettes"
            );
        }
        // Light reads dark on light; dark reads light on dark.
        let floor = palette::CANVAS;
        assert!(
            contrast::relative_luminance(floor.light())
                > contrast::relative_luminance(palette::TEXT_PRIMARY.light())
        );
        assert!(
            contrast::relative_luminance(floor.dark())
                < contrast::relative_luminance(palette::TEXT_PRIMARY.dark())
        );
        assert_eq!(palette::CANVAS.value(Appearance::Dark), dark::CANVAS);
        assert_eq!(palette::CANVAS.value(Appearance::Light), light::CANVAS);
    }

    #[test]
    fn the_dark_layers_climb_from_the_floor_to_the_dialog() {
        // In the dark each layer is lighter than the one under it, so a menu or a
        // dialog is told apart even where its shadow does not show. In the light the
        // content layers are white and the floor and chrome are greyer beneath them.
        for pair in LAYERS.windows(2) {
            let (below_name, below) = pair[0];
            let (above_name, above) = pair[1];
            assert!(
                contrast::relative_luminance(above.dark())
                    > contrast::relative_luminance(below.dark()),
                "dark {above_name} is not above {below_name}"
            );
        }
        for (name, content) in &LAYERS[2..] {
            for (under_name, under) in &LAYERS[..2] {
                assert!(
                    contrast::relative_luminance(content.light())
                        > contrast::relative_luminance(under.light()),
                    "light {name} does not stand on {under_name}"
                );
            }
        }
    }

    /// What one colour looks like laid over another, per sRGB channel.
    fn over(top: u32, alpha: f64, under: u32) -> u32 {
        let channel = |shift: u32| {
            let top = f64::from((top >> shift) & 0xff);
            let under = f64::from((under >> shift) & 0xff);
            (top * alpha + under * (1.0 - alpha)).round() as u32
        };
        channel(16) << 16 | channel(8) << 8 | channel(0)
    }

    #[test]
    fn the_scrim_dims_the_page_it_covers() {
        for (name, scrim, canvas) in [
            ("dark", palette::SCRIM.dark(), palette::CANVAS.dark()),
            ("light", palette::SCRIM.light(), palette::CANVAS.light()),
        ] {
            let covered = over(scrim, f64::from(super::SCRIM_ALPHA), canvas);
            assert!(
                (super::scrim().a - super::SCRIM_ALPHA).abs() < f32::EPSILON,
                "the {name} backdrop is drawn at the documented alpha"
            );
            let before = contrast::relative_luminance(canvas);
            let after = contrast::relative_luminance(covered);
            assert!(
                after <= before * 0.85,
                "the {name} scrim leaves the page at {after:.4} of its {before:.4} luminance"
            );
        }
    }

    #[test]
    fn the_chrome_stays_neutral_and_only_the_accent_carries_hue() {
        let mut neutral = surfaces();
        neutral.extend([
            ("border", palette::BORDER),
            ("border strong", palette::BORDER_STRONG),
            ("text primary", palette::TEXT_PRIMARY),
            ("text secondary", palette::TEXT_SECONDARY),
            ("text muted", palette::TEXT_MUTED),
            ("disabled surface", palette::DISABLED_SURFACE),
            ("disabled text", palette::DISABLED_TEXT),
            ("loading track", palette::LOADING_TRACK),
        ]);
        for (name, token) in neutral {
            for (palette_name, value) in [("dark", token.dark()), ("light", token.light())] {
                let [_, r, g, b] = value.to_be_bytes();
                let spread = u8::max(u8::max(r, g), b) - u8::min(u8::min(r, g), b);
                assert!(
                    spread <= 14,
                    "{palette_name} {name} is {value:06x}, a {spread}-step hue in the chrome"
                );
            }
        }
        // The one identity colour is the logo's periwinkle: blue is its strongest channel
        // and green never drops below red, so the accent cannot turn violet or magenta
        // in a regular control.
        for (name, token) in [
            ("accent", palette::ACCENT),
            ("accent hover", palette::ACCENT_HOVER),
            ("accent pressed", palette::ACCENT_PRESSED),
        ] {
            for (palette_name, value) in [("dark", token.dark()), ("light", token.light())] {
                let [_, r, g, b] = value.to_be_bytes();
                assert!(
                    b > g && g >= r,
                    "{palette_name} {name} is {value:06x}, which is not the logo's periwinkle"
                );
                assert!(
                    b - g >= 0x40,
                    "{palette_name} {name} is {value:06x}, which drifts towards cyan"
                );
            }
        }
        for value in [palette::BRAND_END.dark(), palette::BRAND_END.light()] {
            let [_, r, g, b] = value.to_be_bytes();
            assert!(b > r && b > g, "brand end {value:06x} is not blue-led");
        }
    }

    #[test]
    fn elevation_rises_level_by_level_and_is_deeper_in_the_dark() {
        let reach = |level: Elevation| {
            level
                .shadows()
                .iter()
                .map(|shadow| f32::from(shadow.blur_radius) + f32::from(shadow.offset.y))
                .fold(0.0_f32, f32::max)
        };
        let ink = |level: Elevation| {
            level
                .shadows()
                .iter()
                .map(|shadow| shadow.color.a)
                .fold(0.0_f32, f32::max)
        };
        for appearance in [Appearance::Dark, Appearance::Light] {
            super::set_appearance(appearance);
            assert!(Elevation::Flat.shadows().is_empty());
            assert!(reach(Elevation::Low) < reach(Elevation::Mid));
            assert!(reach(Elevation::Mid) < reach(Elevation::High));
        }
        super::set_appearance(Appearance::Light);
        let light_ink = ink(Elevation::High);
        super::set_appearance(Appearance::Dark);
        let dark_ink = ink(Elevation::High);
        assert!(dark_ink > light_ink, "a dark shadow is not deeper");
    }

    #[test]
    fn motion_durations_step_from_fast_to_slow() {
        assert_eq!(Motion::FAST.as_millis(), 120);
        assert_eq!(Motion::BASE.as_millis(), 180);
        assert_eq!(Motion::SLOW.as_millis(), 260);
        assert!(Enter::Fade.duration() < Enter::Rise.duration());
        assert!(Enter::Rise.duration() < Enter::Settle.duration());
        let ease = super::ease_out();
        assert!(ease(0.0).abs() < f32::EPSILON && (ease(1.0) - 1.0).abs() < f32::EPSILON);
        assert!(ease(0.5) > 0.5, "the entrance decelerates");
    }

    #[test]
    fn the_system_can_only_take_motion_away() {
        assert_eq!(Motion::Full.with_system(false), Motion::Full);
        assert_eq!(Motion::Full.with_system(true), Motion::Reduced);
        assert_eq!(Motion::Reduced.with_system(false), Motion::Reduced);
        assert_eq!(Motion::Reduced.with_system(true), Motion::Reduced);
        assert!(
            Motion::PULSE > Motion::SLOW,
            "a pulse beats slower than an entrance"
        );
        // Tests never read the machine they run on.
        assert!(!super::system_reduces_motion());
        super::fake_system_reduces_motion(true);
        assert!(super::system_reduces_motion());
        super::fake_system_reduces_motion(false);
    }

    /// One object entering over a 200 px block, placed by `place`.
    struct Entrance {
        kind: Enter,
        place: fn(gpui::Div) -> gpui::Div,
    }

    impl gpui::Render for Entrance {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let object =
                (self.place)(gpui::div().size(px(50.0))).debug_selector(|| "entering".to_owned());
            gpui::div()
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .child(gpui::div().h(px(200.0)).w_full().flex_none())
                .child(super::enter(object, "entering", self.kind))
                .child(
                    gpui::div()
                        .h(px(20.0))
                        .w_full()
                        .flex_none()
                        .debug_selector(|| "after".to_owned()),
                )
        }
    }

    fn entering_bounds(
        cx: &mut gpui::TestAppContext,
        kind: Enter,
        place: fn(gpui::Div) -> gpui::Div,
    ) -> (gpui::Bounds<gpui::Pixels>, gpui::Bounds<gpui::Pixels>) {
        super::set_motion(Motion::Full);
        let (_view, cx) = cx.add_window_view(move |_window, _cx| Entrance { kind, place });
        cx.simulate_resize(gpui::size(px(400.0), px(400.0)));
        cx.run_until_parked();
        (
            cx.debug_bounds("entering").expect("the object is drawn"),
            cx.debug_bounds("after").expect("its sibling is drawn"),
        )
    }

    #[gpui::test]
    fn an_entrance_keeps_an_anchored_object_out_of_the_flow(cx: &mut gpui::TestAppContext) {
        for kind in [Enter::Rise, Enter::Settle] {
            // Anchored by its top edge, like a menu under a button.
            let (object, _) = entering_bounds(cx, kind, |object| {
                object.absolute().top(px(40.0)).left(px(30.0))
            });
            assert_eq!(
                object.left(),
                px(30.0),
                "{kind:?} moved the object sideways"
            );
            assert!(
                (f32::from(object.top()) - 40.0).abs() <= kind.travel().abs(),
                "{kind:?} dropped the anchored object back into the flow: {object:?}"
            );
            // Anchored by its bottom edge, like a toast stack.
            let (object, _) = entering_bounds(cx, kind, |object| {
                object.absolute().bottom(px(40.0)).right(px(30.0))
            });
            assert_eq!(object.right(), px(370.0));
            assert!(
                (f32::from(object.bottom()) - 360.0).abs() <= kind.travel().abs(),
                "{kind:?} lost the bottom anchor: {object:?}"
            );
        }
        super::set_motion(Motion::Full);
    }

    #[gpui::test]
    fn an_entrance_in_the_flow_does_not_push_its_siblings(cx: &mut gpui::TestAppContext) {
        for kind in [Enter::Rise, Enter::Settle] {
            let (object, after) = entering_bounds(cx, kind, |object| object);
            assert!(
                (f32::from(object.top()) - 200.0).abs() <= kind.travel().abs(),
                "{kind:?}: {object:?}"
            );
            assert_eq!(after.top(), px(250.0), "{kind:?} moved the next sibling");
        }
    }

    #[test]
    fn reduced_motion_draws_an_entrance_in_its_final_place() {
        super::set_motion(Motion::Reduced);
        let _ = super::enter(gpui::div(), "still", Enter::Rise);
        super::set_motion(Motion::Full);
        let _ = super::enter(gpui::div(), "moving", Enter::Settle);
        assert!(super::motion_allowed());
    }

    #[test]
    fn radii_and_type_grow_with_the_object() {
        let radii = [
            radius::SMALL,
            radius::MEDIUM,
            radius::LARGE,
            radius::XLARGE,
            radius::XXLARGE,
        ];
        assert_eq!(radii, [6.0, 8.0, 10.0, 14.0, 20.0]);
        let titles = [
            type_scale::TITLE,
            type_scale::HEADING,
            type_scale::DISPLAY,
            type_scale::HERO,
        ];
        assert_eq!(titles, [15.0, 17.0, 22.0, 28.0]);
        let metrics = super::Metrics::new(super::Zoom::Largest);
        assert_eq!(metrics.display_size(), gpui::px(44.0));
        assert_eq!(metrics.medium_weight(), gpui::FontWeight::MEDIUM);
    }

    #[test]
    fn every_tone_has_a_mark_of_its_own() {
        let tones = [
            Tone::Neutral,
            Tone::Accent,
            Tone::Positive,
            Tone::Warning,
            Tone::Danger,
            Tone::Planned,
        ];
        let mut marks: Vec<StatusMark> = tones.iter().map(|tone| tone.mark()).collect();
        let count = marks.len();
        marks.sort_by_key(|mark| *mark as u8);
        marks.dedup();
        assert_eq!(count, marks.len(), "two tones are drawn with one mark");
    }

    #[test]
    fn the_light_terminal_keeps_the_accent_cursor() {
        let cursor = TERMINAL
            .iter()
            .find(|(from, _)| *from == 0x8b9ffd)
            .map(|(_, to)| *to)
            .expect("the cursor has a light value");
        let [_, r, g, b] = cursor.to_be_bytes();
        assert!(
            g > r && b > r,
            "the light cursor {cursor:06x} is not the accent"
        );
    }
}
