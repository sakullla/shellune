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
///
/// Most tokens are opaque. The interaction fills ([`palette::SURFACE_HOVER`],
/// [`palette::SURFACE_SELECTED`], [`palette::SURFACE_PRESSED`]) are a veil: white or
/// black at a few percent, laid over whatever layer the element is on, so a hovered
/// row lifts off a panel, a menu and a dialog alike, and a tag on a chosen row is one
/// step lighter again instead of vanishing into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Token {
    dark: u32,
    light: u32,
    /// The opacity in each palette, in percent; 100 for an opaque token.
    dark_alpha: u8,
    light_alpha: u8,
}

impl Token {
    /// A token from its dark and its light value.
    #[must_use]
    pub const fn new(dark: u32, light: u32) -> Self {
        Self {
            dark,
            light,
            dark_alpha: 100,
            light_alpha: 100,
        }
    }

    /// A see-through token: an ink and its opacity in percent, in each palette.
    #[must_use]
    pub const fn veil(dark: (u32, u8), light: (u32, u8)) -> Self {
        Self {
            dark: dark.0,
            light: light.0,
            dark_alpha: dark.1,
            light_alpha: light.1,
        }
    }

    /// The 24-bit sRGB value in the dark palette (a veil's ink, without its opacity).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn dark(self) -> u32 {
        self.dark
    }

    /// The 24-bit sRGB value in the light palette (a veil's ink, without its opacity).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn light(self) -> u32 {
        self.light
    }

    /// The value in one palette (a veil's ink, without its opacity).
    #[must_use]
    pub const fn value(self, appearance: Appearance) -> u32 {
        match appearance {
            Appearance::Dark => self.dark,
            Appearance::Light => self.light,
        }
    }

    /// The opacity in one palette, from 0 to 1.
    #[must_use]
    pub fn alpha(self, appearance: Appearance) -> f32 {
        f32::from(match appearance {
            Appearance::Dark => self.dark_alpha,
            Appearance::Light => self.light_alpha,
        }) / 100.0
    }

    /// Whether the token hides what is under it in both palettes.
    #[must_use]
    pub const fn is_opaque(self) -> bool {
        self.dark_alpha == 100 && self.light_alpha == 100
    }

    /// The opaque colour this token gives laid over `under`, in each palette: what a
    /// veil looks like on a given layer. An opaque token is itself; a veil under it is
    /// first laid on the window's floor.
    #[must_use]
    pub fn over(self, under: Token) -> Token {
        fn blend(top: u32, alpha: u8, under: u32) -> u32 {
            let alpha = f64::from(alpha) / 100.0;
            let channel = |shift: u32| {
                let top = f64::from((top >> shift) & 0xff);
                let under = f64::from((under >> shift) & 0xff);
                // A rounded blend of two bytes is a byte.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let mixed = (top * alpha + under * (1.0 - alpha)).round() as u32;
                mixed
            };
            (channel(16) << 16) | (channel(8) << 8) | channel(0)
        }
        let under = if under.is_opaque() {
            under
        } else {
            under.over(palette::CANVAS)
        };
        Token::new(
            blend(self.dark, self.dark_alpha, under.dark),
            blend(self.light, self.light_alpha, under.light),
        )
    }
}

impl std::fmt::LowerHex for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_opaque() {
            write!(f, "{:06x}/{:06x}", self.dark, self.light)
        } else {
            write!(
                f,
                "{:06x}@{}%/{:06x}@{}%",
                self.dark, self.dark_alpha, self.light, self.light_alpha
            )
        }
    }
}

/// The dark palette: neutral graphite with one periwinkle accent (token v2).
///
/// The surfaces are a short neutral grey ramp - canvas, panel, raised, dialog. The
/// interaction fills are white veils (hover 5%, selected 8%, pressed 10%) that lie over
/// whichever layer they are drawn on; the hairlines are white over the panel (border
/// 7%, strong border 12%), written here as the opaque colour that blend gives. A dark
/// palette tells its layers apart by lightness and edge; the shadows are faint.
pub mod dark {
    /// The window's floor: behind the content area and around the terminal.
    pub const CANVAS: u32 = 0x0f0f11;
    /// The title bar, the tab strip, the sidebar and the status bar: the panel layer.
    pub const CHROME: u32 = 0x151518;
    /// Panels, inputs, rows and cards.
    pub const SURFACE: u32 = 0x151518;
    /// Things that float over content: menus, popovers, tooltips and toasts.
    pub const RAISED: u32 = 0x1c1c20;
    /// Dialogs, the front-most layer.
    pub const OVERLAY: u32 = 0x202024;
    /// White at 5%, over whatever layer it is on.
    pub const SURFACE_HOVER: (u32, u8) = (0xffffff, 5);
    /// White at 8%.
    pub const SURFACE_SELECTED: (u32, u8) = (0xffffff, 8);
    /// White at 10%.
    pub const SURFACE_PRESSED: (u32, u8) = (0xffffff, 10);
    /// White 7% over the panel.
    pub const BORDER: u32 = 0x252528;
    /// White 12% over the panel.
    pub const BORDER_STRONG: u32 = 0x313134;
    pub const SEGMENT_TRACK: u32 = 0x0c0c0e;
    pub const SEGMENT_THUMB: u32 = 0x232327;
    pub const TEXT_PRIMARY: u32 = 0xededef;
    pub const TEXT_SECONDARY: u32 = 0xb0b0b8;
    pub const TEXT_MUTED: u32 = 0x9e9ea6;
    pub const ACCENT: u32 = 0x9396f4;
    pub const ACCENT_HOVER: u32 = 0xa0a3f6;
    pub const ACCENT_PRESSED: u32 = 0x8689f3;
    pub const ON_ACCENT: u32 = 0x0f0f11;
    /// The accent 14% over the panel.
    pub const ACCENT_TINT: u32 = 0x272737;
    pub const ACCENT_ON_TINT: u32 = 0xc5c7fa;
    pub const POSITIVE: u32 = 0x5bc490;
    pub const WARNING: u32 = 0xd9ae5f;
    pub const DANGER: u32 = 0xee8a8a;
    pub const INFO: u32 = 0x78aeeb;
    pub const PLANNED: u32 = 0x9e9ea9;
    /// Each status colour 14% over the panel.
    pub const POSITIVE_TINT: u32 = 0x1f2e29;
    pub const WARNING_TINT: u32 = 0x302a22;
    pub const DANGER_TINT: u32 = 0x332528;
    pub const INFO_TINT: u32 = 0x232a36;
    pub const DANGER_SOLID: u32 = 0xc42b1c;
    pub const ON_DANGER: u32 = 0xffffff;
    pub const TRACK_OFF: u32 = 0x52525b;
    pub const TRACK_OFF_HOVER: u32 = 0x5e5e68;
    pub const TRACK_ON: u32 = 0x6d71e6;
    pub const TRACK_ON_HOVER: u32 = 0x7377ea;
    pub const KNOB: u32 = 0xf4f4f5;
    pub const FOCUS_RING: u32 = 0xa0a3f6;
    pub const DISABLED_SURFACE: u32 = 0x18181b;
    pub const DISABLED_TEXT: u32 = 0x7c7c84;
    pub const LOADING_TRACK: u32 = 0x28282a;
    pub const LOADING_FILL: u32 = 0x9396f4;
    pub const SCRIM: u32 = 0x000000;
    pub const SHADOW: u32 = 0x000000;
    /// The terminal's selection: the accent 32% over the terminal's background.
    pub const SELECTION: u32 = 0x373859;
    /// Eight low-chroma avatar pairs (tint, initials): indigo, steel, teal, sage, moss,
    /// sand, clay and heather, each hue 18% over the panel.
    pub const AVATARS: [(u32, u32); 8] = [
        (0x282836, 0xb5b6e3),
        (0x282f36, 0xb5d0e3),
        (0x283435, 0xb5e3df),
        (0x28342f, 0xb5e3c8),
        (0x2d342a, 0xc8e3b5),
        (0x34302a, 0xe3d4b5),
        (0x342b2a, 0xe3c1b5),
        (0x342830, 0xe3b5cc),
    ];
    pub const TAB_ACCENTS: [u32; 6] = [0x9cbcd8, 0xb9aecb, 0x8fc6cf, 0xaec79a, 0xd3bc9c, 0xd4a8ae];
}

/// The light palette: neutral paper with the same accent, a step deeper.
///
/// The floor is near-white, the panels one step greyer and anything that floats is
/// white; the interaction fills are black veils (hover 4%, selected 6%, pressed 8%)
/// over whichever layer they are on, and the hairlines are black over the panel
/// (border 8%, strong border 14%).
pub mod light {
    pub const CANVAS: u32 = 0xfafafa;
    pub const CHROME: u32 = 0xf4f4f5;
    pub const SURFACE: u32 = 0xf4f4f5;
    pub const RAISED: u32 = 0xffffff;
    pub const OVERLAY: u32 = 0xffffff;
    pub const SURFACE_HOVER: (u32, u8) = (0x000000, 4);
    pub const SURFACE_SELECTED: (u32, u8) = (0x000000, 6);
    pub const SURFACE_PRESSED: (u32, u8) = (0x000000, 8);
    pub const BORDER: u32 = 0xe0e0e1;
    pub const BORDER_STRONG: u32 = 0xd2d2d3;
    pub const SEGMENT_TRACK: u32 = 0xeaeaeb;
    pub const SEGMENT_THUMB: u32 = 0xffffff;
    pub const TEXT_PRIMARY: u32 = 0x18181b;
    pub const TEXT_SECONDARY: u32 = 0x52525b;
    pub const TEXT_MUTED: u32 = 0x63636b;
    pub const ACCENT: u32 = 0x5252d2;
    pub const ACCENT_HOVER: u32 = 0x4747c4;
    pub const ACCENT_PRESSED: u32 = 0x3f3fb5;
    pub const ON_ACCENT: u32 = 0xffffff;
    /// The accent 10% over white.
    pub const ACCENT_TINT: u32 = 0xeeeefa;
    pub const ACCENT_ON_TINT: u32 = 0x3e3eb8;
    pub const POSITIVE: u32 = 0x166c42;
    pub const WARNING: u32 = 0x825400;
    pub const DANGER: u32 = 0xb12f36;
    pub const INFO: u32 = 0x2563ae;
    pub const PLANNED: u32 = 0x5e5e6a;
    /// Each status colour 10% over white.
    pub const POSITIVE_TINT: u32 = 0xe8f0ec;
    pub const WARNING_TINT: u32 = 0xf2eee6;
    pub const DANGER_TINT: u32 = 0xf7eaeb;
    pub const INFO_TINT: u32 = 0xe9eff7;
    pub const DANGER_SOLID: u32 = 0xc42b1c;
    pub const ON_DANGER: u32 = 0xffffff;
    pub const TRACK_OFF: u32 = 0x8a8a93;
    pub const TRACK_OFF_HOVER: u32 = 0x7c7c85;
    pub const TRACK_ON: u32 = 0x5252d2;
    pub const TRACK_ON_HOVER: u32 = 0x4747c4;
    pub const KNOB: u32 = 0xffffff;
    pub const FOCUS_RING: u32 = 0x4747c4;
    pub const DISABLED_SURFACE: u32 = 0xf0f0f1;
    pub const DISABLED_TEXT: u32 = 0x7f7f88;
    pub const LOADING_TRACK: u32 = 0xe5e5e6;
    pub const LOADING_FILL: u32 = 0x5252d2;
    pub const SCRIM: u32 = 0x18181b;
    pub const SHADOW: u32 = 0x000000;
    /// The terminal's selection: the accent 22% over the terminal's background.
    pub const SELECTION: u32 = 0xd7d7f4;
    pub const AVATARS: [(u32, u32); 8] = [
        (0xe5e6ee, 0x2a2b6f),
        (0xe5eaee, 0x2a526f),
        (0xe5eeed, 0x2a6f69),
        (0xe5eee9, 0x2a6f47),
        (0xe9eee5, 0x476f2a),
        (0xeeebe5, 0x6f582a),
        (0xeee7e5, 0x6f3b2a),
        (0xeee5ea, 0x6f2a4d),
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
/// Token v2 (ADR-1): a neutral grey ramp in both palettes and exactly one chromatic
/// identity colour, the periwinkle accent, for the current object, the one primary
/// action and focus. Nothing else in the regular interface is saturated; the status
/// colours are low-chroma and only appear where a state is being stated.
pub mod palette {
    use super::{Token, dark, light};

    macro_rules! tokens {
        ($($(#[$doc:meta])* $name:ident;)*) => {
            $($(#[$doc])* pub const $name: Token = Token::new(dark::$name, light::$name);)*
        };
    }

    tokens! {
        /// The window's floor: the content area and the frame around the terminal.
        CANVAS;
        /// The chrome: the title bar, the tab strip, the sidebar and the status bar.
        /// It is the panel layer, the same value as [`SURFACE`].
        CHROME;
        /// The panel layer: sidebars, inputs, rows and cards.
        SURFACE;
        /// Things that float over content: menus, popovers, tooltips, toasts.
        RAISED;
        /// Dialogs: the front-most surface, drawn over the scrim.
        OVERLAY;
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
        INFO_TINT;
        /// A solid destructive fill, such as the window's close button under the
        /// pointer.
        DANGER_SOLID;
        /// Text and icons on [`DANGER_SOLID`].
        ON_DANGER;
        /// The track of a switch that is off. The knob keeps the graphic floor on it.
        TRACK_OFF;
        /// [`TRACK_OFF`] under the pointer.
        TRACK_OFF_HOVER;
        /// The track of a switch that is on: the accent's hue, deep enough in the dark
        /// that the light knob keeps the graphic floor on it.
        TRACK_ON;
        /// [`TRACK_ON`] under the pointer.
        TRACK_ON_HOVER;
        /// The knob of a switch, in either state.
        KNOB;
        /// The ring around the control the keyboard is on. Always drawn together with a
        /// thicker edge, never instead of one.
        FOCUS_RING;
        /// A control that cannot be used right now.
        DISABLED_SURFACE;
        /// Text on [`DISABLED_SURFACE`]; it keeps the graphic floor.
        DISABLED_TEXT;
        /// The groove a progress bar or an in-flight indicator travels in.
        LOADING_TRACK;
        /// The progress itself.
        LOADING_FILL;
        /// What a dialog lays over the page beneath it, at [`super::SCRIM_ALPHA`].
        SCRIM;
        /// The ink of an elevation shadow.
        SHADOW;
        /// The terminal's selected cells, derived from the accent. It is resolved in the
        /// terminal's own palette, which may differ from the window's.
        SELECTION;
    }

    /// A surface under the pointer: a veil over whatever layer it is on.
    pub const SURFACE_HOVER: Token = Token::veil(dark::SURFACE_HOVER, light::SURFACE_HOVER);
    /// The selected navigation entry, tab, row or choice; also the quiet fill of a tag
    /// or a neutral chip. A veil, so a tag on a chosen row still stands out.
    pub const SURFACE_SELECTED: Token =
        Token::veil(dark::SURFACE_SELECTED, light::SURFACE_SELECTED);
    /// A control under the pressed pointer, one step past selected. A veil.
    pub const SURFACE_PRESSED: Token = Token::veil(dark::SURFACE_PRESSED, light::SURFACE_PRESSED);

    /// Host avatar pairs (tint, initials): eight low-chroma hues.
    ///
    /// An avatar says which host this is, so it must not compete with the accent that
    /// says which host is current, nor with the status colours.
    pub const AVATARS: [(Token, Token); 8] = {
        let mut pairs = [(CANVAS, CANVAS); 8];
        let mut index = 0;
        while index < 8 {
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

/// Spacing scale in logical pixels before zoom is applied: 2, 4, 6, 8, 12, 16, 20, 24
/// and 32, a 4px grid with two half steps for the tightest gaps.
pub mod spacing {
    /// 2px — between stacked lines of one item, a mark and its edge.
    pub const HALF: f32 = 2.0;
    /// 4px — icon to label.
    pub const HAIRLINE: f32 = 4.0;
    /// 6px — inside a chip or a compact control.
    pub const NARROW: f32 = 6.0;
    /// 8px — between controls of one group.
    pub const TIGHT: f32 = 8.0;
    /// 12px — inside a row.
    pub const SNUG: f32 = 12.0;
    /// 16px — panel padding.
    pub const BASE: f32 = 16.0;
    /// 20px — inside a dialog or a status card.
    pub const WIDE: f32 = 20.0;
    /// 24px — between panels.
    pub const LOOSE: f32 = 24.0;
    /// 32px — around a page's content and an empty state.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const SPACIOUS: f32 = 32.0;
}

/// Corner radii in logical pixels: 4, 6, 8 and 12.
///
/// The radius grows with the object: the smaller a thing is, the tighter its corner, so
/// a chip inside a card does not look rounder than the card.
pub mod radius {
    /// Tags, badges, keycaps and checkboxes.
    pub const SMALL: f32 = 4.0;
    /// Buttons, inputs, rows and menu entries.
    pub const MEDIUM: f32 = 6.0;
    /// Cards, menus, toasts, tooltips and the terminal frame.
    pub const LARGE: f32 = 8.0;
    /// Dialogs, the command palette and the splash mark.
    pub const XLARGE: f32 = 12.0;
}

/// Type scale in logical pixels: 12, 13, 14, 16, 20 and 28.
pub mod type_scale {
    /// Captions: hints, metadata, the status bar and shortcut hints.
    pub const CAPTION: f32 = 12.0;
    /// Body copy, list rows and every control's label.
    pub const BODY: f32 = 13.0;
    /// Emphasised body: a card's or a banner's headline.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub const EMPHASIS: f32 = 14.0;
    /// A small heading: a panel or a dialog title.
    pub const TITLE: f32 = 16.0;
    /// A page title.
    pub const HEADING: f32 = 20.0;
    /// The brand: the welcome and the splash.
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
    let appearance = appearance();
    Rgba {
        a: token.alpha(appearance),
        ..rgb(token.value(appearance))
    }
}

/// Resolves a veil laid over `under` as one opaque colour, for an element that must
/// hide what is beneath it (an input method's composition over terminal text) and
/// still wear an interaction fill.
pub fn swatch_over(token: Token, under: Token) -> Rgba {
    swatch(token.over(under))
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
    /// An object entering: a menu, a toast, a card. 200 ms.
    pub const BASE: Duration = Duration::from_millis(200);
    /// A large surface entering: a dialog, the splash. 300 ms.
    pub const SLOW: Duration = Duration::from_millis(300);
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

/// The curve every entrance follows: ease-out-cubic, quick out of the gate and
/// settling softly.
pub fn ease_out() -> impl Fn(f32) -> f32 {
    |progress: f32| {
        let rest = 1.0 - progress.clamp(0.0, 1.0);
        1.0 - rest * rest * rest
    }
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

/// Fades `element` in over `duration` while it travels `travel` logical pixels into
/// place (positive: from below), whatever the motion setting.
///
/// It is for the few arrivals that keep a fade under reduced motion - the start page
/// and the page it hands over to (R3) - where the caller has already chosen the travel
/// for the motion the window draws with: zero for reduced motion, so only the fade is
/// left.
pub fn arrive<E>(
    element: E,
    id: impl Into<ElementId>,
    duration: Duration,
    travel: f32,
) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    arrive_after(element, id, Duration::ZERO, duration, travel)
}

/// [`arrive`] that waits `delay` before it starts, drawn invisible and in its starting
/// place meanwhile: the second of two pieces that arrive one after the other (R3).
pub fn arrive_after<E>(
    element: E,
    id: impl Into<ElementId>,
    delay: Duration,
    duration: Duration,
    travel: f32,
) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    let total = delay + duration;
    let wait = delay.as_secs_f32() / total.as_secs_f32().max(f32::EPSILON);
    element
        .with_animation(
            id,
            Animation::new(total)
                .with_easing(move |progress: f32| delayed_ease_out(progress, wait)),
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

/// The entrance curve over the part of an animation after its first `wait` share:
/// 0 until then, then ease-out-cubic to 1.
#[must_use]
pub fn delayed_ease_out(progress: f32, wait: f32) -> f32 {
    let wait = wait.clamp(0.0, 1.0);
    let local = ((progress - wait) / (1.0 - wait).max(f32::EPSILON)).clamp(0.0, 1.0);
    ease_out()(local)
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

/// How far an object floats above what it sits on: three levels (ADR-1).
///
/// Every level keeps its hairline border; the shadow only adds presence. In the dark a
/// shadow on a near-black floor barely shows, so the layers are told apart by fill and
/// edge and the shadows stay faint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevation {
    /// On the page: only a border. Rows, the chrome, inline controls.
    #[cfg_attr(not(test), allow(dead_code))]
    Flat,
    /// Resting on the page: a border and a soft 0 1 2 shadow. Cards, the primary
    /// button, the chosen segment, a switch's knob.
    Raised,
    /// Over the page: a border and a 0 8 24 shadow. Menus, popovers, tooltips,
    /// toasts, dialogs and the command palette.
    Overlay,
}

impl Elevation {
    /// The shadows of this level in the palette being drawn.
    #[must_use]
    pub fn shadows(self) -> Vec<BoxShadow> {
        let dark = appearance() == Appearance::Dark;
        // (y offset, blur, spread, alpha) for each shadow of the level.
        let layers: &[(f32, f32, f32, f32)] = match (self, dark) {
            (Self::Flat, _) => &[],
            (Self::Raised, false) => &[(1.0, 2.0, 0.0, 0.06)],
            (Self::Raised, true) => &[(1.0, 2.0, 0.0, 0.24)],
            (Self::Overlay, false) => &[(1.0, 2.0, 0.0, 0.04), (8.0, 24.0, 0.0, 0.12)],
            (Self::Overlay, true) => &[(1.0, 2.0, 0.0, 0.20), (8.0, 24.0, 0.0, 0.36)],
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
    /// Neutral information that still wants attention: an inline banner that states a
    /// fact rather than a problem.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    Info,
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
    /// A ring around a dot: information that wants attention.
    Target,
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
            Self::Info => StatusMark::Target,
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
            Self::Info => palette::INFO,
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
            Self::Info => palette::INFO_TINT,
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
}

impl Zoom {
    /// Every zoom level in display order.
    pub const ALL: [Self; 3] = [Self::Compact, Self::Regular, Self::Comfortable];

    /// Multiplier applied to every logical length.
    pub fn factor(self) -> f32 {
        match self {
            Self::Compact => 0.9,
            Self::Regular => 1.0,
            Self::Comfortable => 1.15,
        }
    }

    /// Percent shown in the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "90%",
            Self::Regular => "100%",
            Self::Comfortable => "115%",
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

    /// Height of a compact control inside a toolbar: 28.
    pub fn compact_control_height(self) -> Pixels {
        self.scale(28.0)
    }

    /// Height of a large control: the one call to action of an empty page. 36.
    pub fn large_control_height(self) -> Pixels {
        self.scale(36.0)
    }

    /// Edge length of a square icon control.
    ///
    /// It follows the text size upwards but never below [`HIT_TARGET`]: an icon
    /// button is a pointer target before it is a drawing, and 24×24 is the smallest
    /// that stays comfortable to hit (R4).
    pub fn hit_target(self) -> Pixels {
        self.compact_control_height().max(px(HIT_TARGET))
    }

    /// Height of one tab of the strip: 34 (ADR-7).
    ///
    /// The tabs stand in the middle of the head, so the head's own height leaves an
    /// even margin above and below the current tab's raised pill.
    pub fn tab_height(self) -> Pixels {
        self.scale(34.0)
    }

    /// The narrowest a tab of the strip becomes before the strip scrolls instead (R2).
    pub fn tab_min_width(self) -> Pixels {
        self.scale(120.0)
    }

    /// The narrowest the one tab a compact top row shows becomes (R2, R9).
    ///
    /// A compact row - 200% text in a small window - has no room for a tab of
    /// [`Self::tab_min_width`] beside the controls that must stay on it, so it shows
    /// only the current tab and lets it go this narrow: the state dot, a few letters
    /// of the title and the close button still fit.
    pub fn tab_compact_min_width(self) -> Pixels {
        self.scale(80.0)
    }

    /// The narrowest a name beside a group of tools becomes before the tools wrap
    /// onto a line of their own, rather than squeezing the name to nothing (R4).
    pub fn name_min_width(self) -> Pixels {
        self.scale(96.0)
    }

    /// The widest a tab of the strip grows when there is room (R2).
    pub fn tab_max_width(self) -> Pixels {
        self.scale(220.0)
    }

    /// Body text size.
    pub fn body_size(self) -> Pixels {
        self.scale(type_scale::BODY)
    }

    /// The label of a control - a button, a field, a menu entry - and of a form row:
    /// body size, 13.
    pub fn label_size(self) -> Pixels {
        self.scale(type_scale::BODY)
    }

    /// Emphasised body size: a banner's or a card's headline.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub fn emphasis_size(self) -> Pixels {
        self.scale(type_scale::EMPHASIS)
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
    ///
    /// A veil has no contrast of its own: lay it over its layer first
    /// ([`super::Token::over`]).
    pub fn both(foreground: super::Token, background: super::Token) -> (f64, f64) {
        assert!(
            foreground.is_opaque() && background.is_opaque(),
            "{foreground:x} on {background:x}: lay a veil over its layer before measuring it"
        );
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

/// The guard that keeps views on the tokens (ADR-1).
///
/// It reads the non-test source of `src/app` and checks two things:
///
/// * no colour literal - `rgb(<digit>`, `rgba(<digit>`, `hsla(<digit>` or a six-digit
///   hex value - outside this module (the palettes and the terminal's ANSI table live
///   here) and the capture tool's PNG encoder, whose hex values are a CRC, not colours;
/// * the bare lengths - `scale(<number>)` and `px(<number>)` - do not grow. The
///   baselines below only ever go down as screens move onto named metrics; the count
///   before the migration and each lowered baseline are in
///   `docs/design/02-shell-and-design-system.md` (section 2.5). `px(0)` is not
///   counted: a zero is not a size to name, and renaming `min_w(px(0.0))` to
///   `min_w_0()` must not pass for moving a screen onto the metrics.
#[cfg(test)]
mod guard {
    use std::path::{Path, PathBuf};

    /// Bare `scale(<number>)` calls in the non-test source of `src/app`.
    const SCALE_BASELINE: usize = 13;
    /// Bare `px(<number>)` calls with a number other than zero in the non-test source
    /// of `src/app`.
    const PX_BASELINE: usize = 2;

    /// The files a colour literal is allowed in, and why.
    const COLOUR_HOMES: [&str; 2] = [
        // The palettes, the avatar and tab tables and the terminal's ANSI colours.
        "theme.rs",
        // The PNG writer's CRC table, not a colour.
        "capture.rs",
    ];

    fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir).expect("the source directory is readable");
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                found.push(path);
            }
        }
    }

    /// One line without its string and char literals and without a line comment.
    fn code_of(line: &str) -> String {
        let chars: Vec<char> = line.chars().collect();
        let mut code = String::new();
        let mut index = 0;
        while index < chars.len() {
            let ch = chars[index];
            if ch == '/' && chars.get(index + 1) == Some(&'/') {
                break;
            }
            if ch == '"' {
                index += 1;
                while index < chars.len() && chars[index] != '"' {
                    if chars[index] == '\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
                continue;
            }
            if ch == '\'' {
                if chars.get(index + 1).is_some_and(|next| *next != '\\')
                    && chars.get(index + 2) == Some(&'\'')
                {
                    index += 3;
                    continue;
                }
                if chars.get(index + 1) == Some(&'\\')
                    && let Some(end) = chars[index + 2..].iter().position(|c| *c == '\'')
                {
                    index += end + 3;
                    continue;
                }
            }
            code.push(ch);
            index += 1;
        }
        code
    }

    /// The source without the items under `#[cfg(test)]`, as code only.
    fn non_test_code(text: &str) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let mut kept = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            if lines[index].trim() == "#[cfg(test)]" {
                let mut next = index + 1;
                while next < lines.len() && lines[next].trim().starts_with("#[") {
                    next += 1;
                }
                let mut depth = 0_i32;
                let mut opened = false;
                while next < lines.len() {
                    let code = code_of(lines[next]);
                    for ch in code.chars() {
                        match ch {
                            '{' => {
                                depth += 1;
                                opened = true;
                            }
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                    next += 1;
                    if (opened && depth <= 0) || (!opened && code.trim_end().ends_with(';')) {
                        break;
                    }
                }
                index = next;
                continue;
            }
            kept.push(code_of(lines[index]));
            index += 1;
        }
        kept.join("\n")
    }

    /// Whether `code` at `at` starts a call `name(` that is not the tail of a longer
    /// identifier.
    fn call_at(code: &[u8], at: usize, name: &str) -> Option<usize> {
        let end = at + name.len();
        if code.get(at..end) != Some(name.as_bytes()) || code.get(end) != Some(&b'(') {
            return None;
        }
        if at > 0 && (code[at - 1].is_ascii_alphanumeric() || code[at - 1] == b'_') {
            return None;
        }
        Some(end + 1)
    }

    /// Whether a bare number - `-?digits[_digits][.digits]` - and the closing
    /// parenthesis follow `from`.
    fn bare_number(code: &[u8], from: usize) -> bool {
        bare_value(code, from).is_some()
    }

    /// Whether a bare number other than zero and the closing parenthesis follow `from`.
    fn bare_length(code: &[u8], from: usize) -> bool {
        bare_value(code, from).is_some_and(|zero| !zero)
    }

    /// The bare number that follows `from` up to the closing parenthesis, if there is
    /// one: whether it is zero.
    fn bare_value(code: &[u8], from: usize) -> Option<bool> {
        let mut at = from;
        while code.get(at) == Some(&b' ') {
            at += 1;
        }
        if code.get(at) == Some(&b'-') {
            at += 1;
        }
        let digits = at;
        while code
            .get(at)
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'_')
        {
            at += 1;
        }
        if at == digits || !code[digits].is_ascii_digit() {
            return None;
        }
        let mut zero = code[digits..at]
            .iter()
            .all(|byte| matches!(byte, b'0' | b'_'));
        if code.get(at) == Some(&b'.') {
            at += 1;
            let fraction = at;
            while code.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            if at == fraction {
                return None;
            }
            zero &= code[fraction..at].iter().all(|byte| *byte == b'0');
        }
        while code.get(at) == Some(&b' ') {
            at += 1;
        }
        (code.get(at) == Some(&b')')).then_some(zero)
    }

    /// What one file holds: bare `scale`s, bare `px`es and colour literals.
    fn tally(code: &str) -> (usize, usize, Vec<String>) {
        let bytes = code.as_bytes();
        let (mut scale, mut px) = (0, 0);
        let mut colours = Vec::new();
        for at in 0..bytes.len() {
            if let Some(open) = call_at(bytes, at, "scale")
                && bare_number(bytes, open)
            {
                scale += 1;
            }
            if let Some(open) = call_at(bytes, at, "px")
                && bare_length(bytes, open)
            {
                px += 1;
            }
            for name in ["rgb", "rgba", "hsla"] {
                if let Some(open) = call_at(bytes, at, name) {
                    let mut first = open;
                    while bytes.get(first) == Some(&b' ') {
                        first += 1;
                    }
                    if bytes.get(first).is_some_and(u8::is_ascii_digit) {
                        colours.push(format!(
                            "{name}({}",
                            &code[first..(first + 8).min(code.len())]
                        ));
                    }
                }
            }
            if bytes.get(at..at + 2) == Some(b"0x")
                && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric())
            {
                let digits = bytes[at + 2..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_hexdigit() || **byte == b'_')
                    .filter(|byte| **byte != b'_')
                    .count();
                if digits >= 6 {
                    colours.push(code[at..(at + 12).min(code.len())].to_owned());
                }
            }
        }
        (scale, px, colours)
    }

    /// The bare lengths and the stray colours of the whole of `src/app`.
    fn survey() -> (usize, usize, Vec<String>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("app");
        let mut files = Vec::new();
        sources(&root, &mut files);
        files.sort();
        let (mut scale, mut px) = (0, 0);
        let mut stray = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).expect("a source file is readable");
            let (file_scale, file_px, colours) = tally(&non_test_code(&text));
            scale += file_scale;
            px += file_px;
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !COLOUR_HOMES.contains(&name) {
                stray.extend(
                    colours
                        .into_iter()
                        .map(|colour| format!("{}: {colour}", file.display())),
                );
            }
        }
        (scale, px, stray)
    }

    #[test]
    fn views_name_tokens_instead_of_colour_literals() {
        let (_, _, stray) = survey();
        assert!(
            stray.is_empty(),
            "colour literals outside the theme: {stray:#?}"
        );
    }

    #[test]
    fn bare_lengths_only_ever_go_down() {
        let (scale, px, _) = survey();
        eprintln!("bare lengths: {scale} scale(<number>), {px} px(<number other than 0>)");
        assert!(
            scale <= SCALE_BASELINE,
            "{scale} bare scale(<number>) calls, above the baseline of {SCALE_BASELINE}: \
             name the length with a metric or a spacing token"
        );
        assert!(
            px <= PX_BASELINE,
            "{px} bare px(<number>) calls, above the baseline of {PX_BASELINE}: name the \
             length with a metric or a spacing token"
        );
    }

    #[test]
    fn the_guard_reads_calls_and_skips_tests_strings_and_comments() {
        let source = "\
            fn view() { div().w(px(4.0)).h(metrics.scale(12.0)).gap(px(spacing::TIGHT)) }\n\
            // px(9.0) in a comment\n\
            fn named() { let _ = \"px(3.0) in a string\"; let _ = rgb(0x123456); }\n\
            #[cfg(test)]\n\
            mod tests {\n\
                fn t() { px(1.0); scale(2.0); }\n\
            }\n\
            fn after() { metrics.scale(-1.5); my_px(2.0); }\n\
            fn zero() { div().min_w(px(0.0)).top(px(0.)).left(px(0)).w(px(0.5)) }\n";
        let (scale, px, colours) = tally(&non_test_code(source));
        assert_eq!(scale, 2, "scale(12.0) and scale(-1.5)");
        assert_eq!(px, 2, "px(4.0) and px(0.5); a zero is not a length");
        assert_eq!(colours.len(), 2, "rgb(0x…) and its hex value: {colours:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Appearance, Elevation, Enter, Motion, StatusMark, Token, Tone, contrast, dark,
        light::{self, TERMINAL},
        palette, radius, spacing, type_scale,
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

    /// The interaction fills: veils over whatever layer they are on.
    const VEILS: [(&str, Token); 3] = [
        ("hover", palette::SURFACE_HOVER),
        ("selected", palette::SURFACE_SELECTED),
        ("pressed", palette::SURFACE_PRESSED),
    ];

    /// Every surface a label can be drawn on: the layers, and each interaction fill
    /// laid over each layer - a hovered menu item, a chosen row in a dialog.
    fn surfaces() -> Vec<(String, Token)> {
        let mut surfaces: Vec<(String, Token)> = LAYERS
            .iter()
            .map(|(name, token)| ((*name).to_owned(), *token))
            .collect();
        for (veil, token) in VEILS {
            for (layer, under) in LAYERS {
                surfaces.push((format!("{veil} on {layer}"), token.over(under)));
            }
        }
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
        for (name, token) in LAYERS.into_iter().chain(VEILS) {
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
        // In the dark each layer is at least as light as the one under it - the chrome
        // is the panel layer - and the floor, the panel, the floating layer and the
        // dialog each step up, so a menu or a dialog is told apart even where its
        // shadow does not show.
        for pair in LAYERS.windows(2) {
            let (below_name, below) = pair[0];
            let (above_name, above) = pair[1];
            assert!(
                contrast::relative_luminance(above.dark())
                    >= contrast::relative_luminance(below.dark()),
                "dark {above_name} is below {below_name}"
            );
        }
        for (below, above) in [
            (palette::CANVAS, palette::SURFACE),
            (palette::SURFACE, palette::RAISED),
            (palette::RAISED, palette::OVERLAY),
        ] {
            assert!(
                contrast::relative_luminance(above.dark())
                    > contrast::relative_luminance(below.dark()),
                "dark {above:x} does not step up from {below:x}"
            );
        }
        // In the light the panels are one step greyer than the floor and whatever
        // floats is white, so it stands on both.
        assert_ne!(palette::SURFACE.light(), palette::CANVAS.light());
        for floating in [palette::RAISED, palette::OVERLAY] {
            for under in [palette::CANVAS, palette::SURFACE] {
                assert!(
                    contrast::relative_luminance(floating.light())
                        > contrast::relative_luminance(under.light()),
                    "light {floating:x} does not stand on {under:x}"
                );
            }
        }
    }

    #[test]
    fn the_interaction_fills_and_hairlines_are_the_documented_blends() {
        // Token v2 (ADR-1): hover, selected and pressed are white (dark) or black
        // (light) veils at a few percent, drawn see-through over whatever layer they
        // are on; the two hairlines are the same inks laid over the panel, held as the
        // opaque result. This keeps the tables and the design document in step.
        for (name, token, dark_alpha, light_alpha) in [
            ("hover", palette::SURFACE_HOVER, 0.05, 0.04),
            ("selected", palette::SURFACE_SELECTED, 0.08, 0.06),
            ("pressed", palette::SURFACE_PRESSED, 0.10, 0.08),
        ] {
            assert!(!token.is_opaque(), "{name} is a veil");
            assert_eq!(token.dark(), 0xffffff, "dark {name} is white");
            assert_eq!(token.light(), 0x000000, "light {name} is black");
            assert!((f64::from(token.alpha(Appearance::Dark)) - dark_alpha).abs() < 1e-6);
            assert!((f64::from(token.alpha(Appearance::Light)) - light_alpha).abs() < 1e-6);
            // Over the panel the veil is the documented blend.
            assert_eq!(
                token.over(palette::SURFACE).dark(),
                over(0xffffff, dark_alpha, palette::SURFACE.dark()),
                "dark {name}"
            );
            assert_eq!(
                token.over(palette::SURFACE).light(),
                over(0x000000, light_alpha, palette::SURFACE.light()),
                "light {name}"
            );
        }
        let surface_dark = palette::SURFACE.dark();
        let surface_light = palette::SURFACE.light();
        for (name, token, dark_alpha, light_alpha) in [
            ("border", palette::BORDER, 0.07, 0.08),
            ("border strong", palette::BORDER_STRONG, 0.12, 0.14),
        ] {
            assert_eq!(
                token.dark(),
                over(0xffffff, dark_alpha, surface_dark),
                "dark {name}"
            );
            assert_eq!(
                token.light(),
                over(0x000000, light_alpha, surface_light),
                "light {name}"
            );
        }
        // The terminal's selection is the accent over the terminal's own background.
        assert_eq!(
            palette::SELECTION.dark(),
            over(dark::ACCENT, 0.32, 0x0b0c10)
        );
        assert_eq!(
            palette::SELECTION.light(),
            over(light::ACCENT, 0.22, 0xfcfcfd)
        );
    }

    /// The least lightness step, as a contrast ratio, that reads as "this changed":
    /// a hovered item against its layer, a tag against the row it is on.
    const FILL_STEP: f64 = 1.08;

    #[test]
    fn every_interaction_fill_stands_off_every_layer_it_is_drawn_on() {
        // The fills were once opaque blends over the panel. On a menu or a dialog the
        // dark hover came out at 1.01:1 against the layer, so a hovered menu item or
        // a dialog's ghost button did not change, and a tag (the selected fill) on a
        // chosen row (the same fill) vanished. As veils each one steps off whatever
        // it is on.
        for (layer_name, layer) in LAYERS {
            for (veil_name, veil) in VEILS {
                let (dark, light) = contrast::both(veil.over(layer), layer);
                assert!(
                    dark >= FILL_STEP && light >= FILL_STEP,
                    "{veil_name} on {layer_name} is {dark:.3}:1 dark and {light:.3}:1 \
                     light against the layer"
                );
            }
            // A tag on a chosen row: the selected fill over the selected fill.
            let row = palette::SURFACE_SELECTED.over(layer);
            let tag = palette::SURFACE_SELECTED.over(row);
            let (dark, light) = contrast::both(tag, row);
            assert!(
                dark >= FILL_STEP && light >= FILL_STEP,
                "a tag on a chosen row on {layer_name} is {dark:.3}:1 / {light:.3}:1"
            );
        }
    }

    #[test]
    fn a_veil_is_drawn_see_through_and_composites_to_an_opaque_colour() {
        super::set_appearance(Appearance::Dark);
        let hover = super::swatch(palette::SURFACE_HOVER);
        assert!((hover.a - 0.05).abs() < 1e-6, "{hover:?}");
        assert!((super::swatch(palette::CANVAS).a - 1.0).abs() < f32::EPSILON);
        let laid = super::swatch_over(palette::SURFACE_SELECTED, palette::SURFACE);
        assert!((laid.a - 1.0).abs() < f32::EPSILON);
        assert!(palette::SURFACE_SELECTED.over(palette::OVERLAY).is_opaque());
        assert_eq!(palette::CANVAS.over(palette::OVERLAY), palette::CANVAS);
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
        neutral.extend(
            [
                ("border", palette::BORDER),
                ("border strong", palette::BORDER_STRONG),
                ("text primary", palette::TEXT_PRIMARY),
                ("text secondary", palette::TEXT_SECONDARY),
                ("text muted", palette::TEXT_MUTED),
                ("disabled surface", palette::DISABLED_SURFACE),
                ("disabled text", palette::DISABLED_TEXT),
                ("loading track", palette::LOADING_TRACK),
            ]
            .map(|(name, token)| (name.to_owned(), token)),
        );
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
    }

    #[test]
    fn elevation_rises_level_by_level() {
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
            assert!(reach(Elevation::Raised) < reach(Elevation::Overlay));
            assert!(ink(Elevation::Raised) <= ink(Elevation::Overlay));
            // Raised is the soft 0 1 2 and the overlay reaches 0 8 24.
            assert!((reach(Elevation::Raised) - 3.0).abs() < f32::EPSILON);
            assert!((reach(Elevation::Overlay) - 32.0).abs() < f32::EPSILON);
        }
        // A shadow on the dark floor stays faint: the layers are told apart by fill
        // and edge there.
        super::set_appearance(Appearance::Dark);
        assert!(
            ink(Elevation::Overlay) <= 0.4,
            "the dark shadow is not faint"
        );
    }

    #[test]
    fn motion_durations_step_from_fast_to_slow() {
        assert_eq!(Motion::FAST.as_millis(), 120);
        assert_eq!(Motion::BASE.as_millis(), 200);
        assert_eq!(Motion::SLOW.as_millis(), 300);
        assert!(Enter::Fade.duration() < Enter::Rise.duration());
        assert!(Enter::Rise.duration() < Enter::Settle.duration());
        let ease = super::ease_out();
        assert!(ease(0.0).abs() < f32::EPSILON && (ease(1.0) - 1.0).abs() < f32::EPSILON);
        assert!(ease(0.5) > 0.5, "the entrance decelerates");
        // Ease-out-cubic: 1 - (1 - t)^3.
        assert!((ease(0.5) - 0.875).abs() < 1e-6);
    }

    #[test]
    fn a_delayed_entrance_holds_still_then_follows_the_same_curve() {
        let wait = 40.0 / 340.0;
        assert!(super::delayed_ease_out(0.0, wait).abs() < f32::EPSILON);
        assert!(super::delayed_ease_out(wait, wait).abs() < f32::EPSILON);
        assert!((super::delayed_ease_out(1.0, wait) - 1.0).abs() < f32::EPSILON);
        let halfway = wait + (1.0 - wait) / 2.0;
        assert!((super::delayed_ease_out(halfway, wait) - 0.875).abs() < 1e-5);
        assert!(
            (super::delayed_ease_out(0.5, 0.0) - super::ease_out()(0.5)).abs() < f32::EPSILON,
            "without a wait it is the plain entrance"
        );
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
        let radii = [radius::SMALL, radius::MEDIUM, radius::LARGE, radius::XLARGE];
        assert_eq!(radii, [4.0, 6.0, 8.0, 12.0]);
        let sizes = [
            type_scale::CAPTION,
            type_scale::BODY,
            type_scale::EMPHASIS,
            type_scale::TITLE,
            type_scale::HEADING,
            type_scale::HERO,
        ];
        assert_eq!(sizes, [12.0, 13.0, 14.0, 16.0, 20.0, 28.0]);
        let steps = [
            spacing::HALF,
            spacing::HAIRLINE,
            spacing::NARROW,
            spacing::TIGHT,
            spacing::SNUG,
            spacing::BASE,
            spacing::WIDE,
            spacing::LOOSE,
            spacing::SPACIOUS,
        ];
        assert_eq!(steps, [2.0, 4.0, 6.0, 8.0, 12.0, 16.0, 20.0, 24.0, 32.0]);
        let metrics = super::Metrics::new(super::Zoom::Comfortable);
        assert!(metrics.heading_size() > super::Metrics::default().heading_size());
        assert_eq!(metrics.label_size(), metrics.body_size());
        assert_eq!(metrics.medium_weight(), gpui::FontWeight::MEDIUM);
        let regular = super::Metrics::default();
        assert_eq!(
            [
                regular.compact_control_height(),
                regular.control_height(),
                regular.large_control_height(),
            ],
            [gpui::px(28.0), gpui::px(32.0), gpui::px(36.0)]
        );
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
            Tone::Info,
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
