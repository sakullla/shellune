//! Design tokens for the Shellune shell (R12, R13).
//!
//! The values here are the single source of truth for the palette, spacing,
//! corner radii, type scale and zoom behaviour described in
//! `docs/design/02-shell-and-design-system.md`. Views reference tokens instead of literal
//! colours or sizes, so a visual review only has to change this module.
//!
//! Four properties are checked by tests rather than trusted:
//!
//! * every text and status colour reaches the WCAG contrast floor against the
//!   surface it is drawn on ([`contrast`]), in both palettes;
//! * every token has exactly one light value and no two tokens share a value, so the
//!   dark-keyed table in [`light`] can resolve all of them;
//! * the chrome stays neutral and the accent is the only identity hue in regular
//!   use, which is what keeps two themes looking like one product;
//! * layout metrics shrink and grow with [`Zoom`] instead of being hard-coded
//!   per view.

use gpui::{FontWeight, Pixels, Rgba, px, rgb};
use shellune_core::{SessionFailure, SessionKind, SessionState, TabAccent, TransferState};

/// Raw colour tokens as 24-bit sRGB values.
///
/// The palette is neutral graphite in the dark and soft paper white in the light,
/// with exactly one chromatic identity colour: a cyan-blue accent for the current
/// object and the one primary action. Nothing else in the regular interface is
/// allowed to be saturated, so a busy session list stays quiet and the only colours
/// that shout are the four status tones.
pub mod palette {
    /// Application background: the content area and the terminal itself.
    pub const CANVAS: u32 = 0x0b0c10;
    /// Chrome surface: the navigation rail, the tab strip and the status bar.
    pub const SURFACE: u32 = 0x121317;
    /// Raised surface for cards, rows and inputs.
    pub const SURFACE_RAISED: u32 = 0x17181d;
    /// Surface under the pointer, one step above [`SURFACE_RAISED`].
    pub const SURFACE_HOVER: u32 = 0x1e2026;
    /// Surface used for the selected navigation entry, tab or choice.
    pub const SURFACE_SELECTED: u32 = 0x26282f;
    /// Surface of a control under the pressed pointer: a step below
    /// [`SURFACE_SELECTED`], so a press is felt and not only tinted.
    pub const SURFACE_PRESSED: u32 = 0x2e3038;
    /// Hairline border between regions.
    pub const BORDER: u32 = 0x24262d;
    /// Border of controls and of an element under the pointer.
    pub const BORDER_STRONG: u32 = 0x383b44;
    /// The sunken track a segmented control's options sit in.
    pub const SEGMENT_TRACK: u32 = 0x0e0f13;
    /// The raised thumb under the chosen option of a segmented control.
    pub const SEGMENT_THUMB: u32 = 0x282a32;
    /// Primary text: titles and the current object.
    pub const TEXT_PRIMARY: u32 = 0xecedf1;
    /// Secondary text: body copy and list metadata.
    pub const TEXT_SECONDARY: u32 = 0xaeb2bb;
    /// Muted text: hints, counts and the status bar.
    ///
    /// Light enough to keep the body floor on [`SURFACE_PRESSED`], the darkest
    /// surface a muted label is drawn on, not just on the raised surfaces.
    pub const TEXT_MUTED: u32 = 0x9498a2;
    /// Accent used for the selected destination and primary actions.
    pub const ACCENT: u32 = 0x4cc2dd;
    /// Accent under the pointer, for a solid primary button.
    pub const ACCENT_HOVER: u32 = 0x6bd4ec;
    /// The accent under a pressed primary button: one step down from [`ACCENT`], so a
    /// press sinks instead of brightening.
    pub const ACCENT_PRESSED: u32 = 0x37a9c4;
    /// Text drawn on a solid [`ACCENT`] fill.
    pub const ON_ACCENT: u32 = 0x04161c;
    /// Accent tint for filled accent surfaces.
    pub const ACCENT_TINT: u32 = 0x0f2b33;
    /// Text and icons drawn on [`ACCENT_TINT`].
    pub const ACCENT_ON_TINT: u32 = 0x9fe3f2;
    /// Dark tint behind a positive chip or banner.
    pub const POSITIVE_TINT: u32 = 0x11241d;
    /// Dark tint behind a warning chip or banner.
    pub const WARNING_TINT: u32 = 0x261f12;
    /// Dark tint behind a danger chip, banner or destructive button.
    pub const DANGER_TINT: u32 = 0x2b1618;
    /// Live and completed status.
    pub const POSITIVE: u32 = 0x5ed6a4;
    /// Waiting, verifying or in-progress status.
    pub const WARNING: u32 = 0xe8c07a;
    /// Failed, rejected or unavailable status.
    pub const DANGER: u32 = 0xff8f96;
    /// Planned-but-not-shipped status; deliberately desaturated so it never
    /// reads as an error or as success.
    pub const PLANNED: u32 = 0x9aa0ab;
    /// The ring drawn around the control the keyboard is on.
    ///
    /// It is the accent's own family but its own token: a focus ring may be tuned
    /// for visibility without moving the colour of the primary action. The ring is
    /// always drawn in addition to a thickness or edge change, never instead of one.
    pub const FOCUS_RING: u32 = 0x7fdcf1;
    /// Fill of a control that cannot be used right now.
    pub const DISABLED_SURFACE: u32 = 0x141519;
    /// Text and icons on [`DISABLED_SURFACE`].
    ///
    /// Inactive controls are exempt from WCAG AA, but they are still read, so the
    /// pair keeps the graphic floor instead of disappearing into the fill.
    pub const DISABLED_TEXT: u32 = 0x7a7e87;
    /// The groove an in-flight indicator travels in.
    pub const LOADING_TRACK: u32 = 0x22242b;
    /// The in-flight indicator itself; it never carries meaning on its own, the
    /// state's word does.
    pub const LOADING_FILL: u32 = 0x59c9e3;
    /// What a dialog lays over the page beneath it, at [`scrim`]'s alpha.
    pub const SCRIM: u32 = 0x05060a;
    /// Host avatar pairs (tint, initials): slate, steel, sand, sage, clay.
    ///
    /// The first pair is the accent tint, kept for the first host group. The rest are
    /// low-chroma earth and metal tints: an avatar says which host this is, so it must
    /// not compete with the accent that says which host is current.
    pub const AVATARS: [(u32, u32); 6] = [
        (ACCENT_TINT, ACCENT_ON_TINT),
        (0x1a2430, 0xbcd2e6),
        (0x16262a, 0xa9d0cb),
        (0x282219, 0xe0cfa4),
        (0x1e2620, 0xb3cfb0),
        (0x2a1f1d, 0xe0bcae),
    ];
    /// The deeper stop of the one remaining accent gradient.
    ///
    /// It stays in the accent's own family: a violet second stop is gone, and no
    /// regular control may be filled with a gradient - only the welcome emblem still
    /// uses two stops of the same cyan.
    pub const BRAND_END: u32 = 0x1f7f96;
    /// Tab identity colours, in [`TabAccent::ALL`] order.
    ///
    /// They are deliberately not the status hues: green, amber and red keep meaning
    /// "connected", "waiting" and "failed", and a tab colour must never be read as
    /// one of those. Each value keeps its contrast on every tab surface in both
    /// palettes (checked by the palette tests).
    pub const TAB_ACCENTS: [u32; 6] = [
        0x9cbcd8, // slate
        0xb9aecb, // heather
        0x8fc6cf, // steel
        0xaec79a, // sage
        0xd3bc9c, // sand
        0xd4a8ae, // dust
    ];
}

/// The token a tab's distinguishing colour is drawn with.
///
/// The colour is an identity aid, never a status: a tab keeps its status dot, its
/// state word and its target whatever colour the user gives it.
#[must_use]
pub fn tab_accent_token(accent: TabAccent) -> u32 {
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
    pub fill: Option<u32>,
    /// The ring around the colour, when this state draws one.
    pub ring: Option<u32>,
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

/// Corner radii in logical pixels.
pub mod radius {
    /// Chips, keycaps and small controls.
    pub const SMALL: f32 = 6.0;
    /// Buttons, rows, tabs and inputs.
    pub const MEDIUM: f32 = 8.0;
    /// Cards, menus and the terminal frame.
    pub const LARGE: f32 = 12.0;
    /// Hero tiles and the welcome emblem.
    pub const XLARGE: f32 = 16.0;
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
    /// Destination title in the header.
    pub const HEADING: f32 = 17.0;
    /// The welcome title on an empty page.
    pub const HERO: f32 = 24.0;
}

/// Colour helper so views do not call `rgb` with literals.
pub fn swatch(hex: u32) -> Rgba {
    rgb(resolve(hex))
}

/// Which palette the window is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    /// The dark palette: the tokens' own values.
    #[default]
    Dark,
    /// The light palette, looked up from the dark tokens.
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
/// This is an application setting. GPUI 0.2.2's window appearance is observed for
/// light and dark, but this crate has no evidence of a system reduced-motion
/// query, so the shell does not claim to follow one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Play short, skippable feedback such as a loading dot fading in.
    #[default]
    Full,
    /// Skip that motion. Status text, colour and available actions stay.
    Reduced,
}

impl Motion {
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
    /// The motion preference of the window being drawn. Not read from the OS.
    static MOTION: std::cell::Cell<Motion> = const { std::cell::Cell::new(Motion::Full) };
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

/// Sets the motion preference the next draws use. This does not consult the OS.
pub fn set_motion(motion: Motion) {
    MOTION.with(|current| current.set(motion));
}

/// The value a token has in the current palette.
///
/// The tokens are the dark palette's values; the light palette is a table keyed by
/// them, so every view keeps naming one token and gets the value that fits.
#[must_use]
pub fn resolve(token: u32) -> u32 {
    match appearance() {
        Appearance::Dark => token,
        Appearance::Light => light_value(token),
    }
}

/// The light value of a dark token; a value that is not a token is returned as it is.
#[must_use]
pub fn light_value(token: u32) -> u32 {
    light::TABLE
        .iter()
        .find(|(dark, _)| *dark == token)
        .map_or(token, |(_, light)| *light)
}

/// The light palette, as pairs of (dark token, light value).
pub mod light {
    use super::palette as dark;

    /// Every token of [`super::palette`] and its light value.
    pub const TABLE: &[(u32, u32)] = &[
        (dark::CANVAS, 0xfcfcfd),
        (dark::SURFACE, 0xf4f5f7),
        (dark::SURFACE_RAISED, 0xffffff),
        (dark::SURFACE_HOVER, 0xeeeff2),
        (dark::SURFACE_SELECTED, 0xe4e7ea),
        (dark::SURFACE_PRESSED, 0xdcdfe4),
        (dark::BORDER, 0xe3e5e9),
        (dark::BORDER_STRONG, 0xc9cdd4),
        (dark::SEGMENT_TRACK, 0xeef0f2),
        (dark::SEGMENT_THUMB, 0xffffff),
        (dark::TEXT_PRIMARY, 0x14161a),
        (dark::TEXT_SECONDARY, 0x454a53),
        (dark::TEXT_MUTED, 0x5c616a),
        (dark::ACCENT, 0x0d6b82),
        (dark::ACCENT_HOVER, 0x0a5a6e),
        (dark::ACCENT_PRESSED, 0x084f60),
        (dark::ON_ACCENT, 0xffffff),
        (dark::ACCENT_TINT, 0xe2f1f6),
        (dark::ACCENT_ON_TINT, 0x0a5568),
        (dark::POSITIVE_TINT, 0xdff2e8),
        (dark::WARNING_TINT, 0xfaf0d8),
        (dark::DANGER_TINT, 0xfce7e9),
        (dark::POSITIVE, 0x10714b),
        (dark::WARNING, 0x7a5300),
        (dark::DANGER, 0xb32b3c),
        (dark::PLANNED, 0x5b606a),
        (dark::FOCUS_RING, 0x0a5a6e),
        (dark::DISABLED_SURFACE, 0xf1f2f4),
        (dark::DISABLED_TEXT, 0x777c85),
        (dark::LOADING_TRACK, 0xe2e4e8),
        (dark::LOADING_FILL, 0x0d6b82),
        (dark::SCRIM, 0x1a1c20),
        (dark::BRAND_END, 0x0a5a6e),
        // Host avatars after the accent pair they share their first entry with.
        (dark::AVATARS[1].0, 0xe4eef7),
        (dark::AVATARS[1].1, 0x1c4a6b),
        (dark::AVATARS[2].0, 0xdff1ee),
        (dark::AVATARS[2].1, 0x14574f),
        (dark::AVATARS[3].0, 0xf8efd9),
        (dark::AVATARS[3].1, 0x6b4a00),
        (dark::AVATARS[4].0, 0xe6f1e4),
        (dark::AVATARS[4].1, 0x2d5a2a),
        (dark::AVATARS[5].0, 0xf9e9e3),
        (dark::AVATARS[5].1, 0x7a3a24),
        // Tab identity colours: the same hue, dark enough for a light surface.
        (dark::TAB_ACCENTS[0], 0x2f5f86),
        (dark::TAB_ACCENTS[1], 0x5d4a80),
        (dark::TAB_ACCENTS[2], 0x1a6b74),
        (dark::TAB_ACCENTS[3], 0x51662f),
        (dark::TAB_ACCENTS[4], 0x8a5a20),
        (dark::TAB_ACCENTS[5], 0x8b4450),
    ];

    /// The terminal's default colours and its 16 ANSI colours, light.
    ///
    /// Keyed by the dark values of `shellune_terminal::palette`; a colour a program
    /// set itself (256-colour or true colour) is drawn as it asked.
    pub const TERMINAL: &[(u32, u32)] = &[
        (0xe6e9f0, 0x1f2329), // foreground
        (0x0b0c10, 0xfcfcfd), // background
        (0x8fa0ff, 0x0d6b82), // cursor
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
#[cfg_attr(not(test), allow(dead_code))]
pub const SCRIM_ALPHA: f32 = 0.62;

/// The backdrop a dialog draws over the page beneath it.
///
/// It is a translucent [`palette::SCRIM`] rather than a solid fill, so a dialog does
/// not look like a second window and the page keeps its place.
#[cfg_attr(not(test), allow(dead_code))]
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

    /// The raw token behind [`Tone::text`], exposed for contrast tests.
    pub fn token(self) -> u32 {
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

    /// The raw token behind [`Tone::chip_background`].
    pub fn tint(self) -> u32 {
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

    /// Height of the session bar above a terminal.
    pub fn session_bar_height(self) -> Pixels {
        self.scale(36.0)
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

    /// Welcome title size.
    pub fn hero_size(self) -> Pixels {
        self.scale(type_scale::HERO)
    }

    /// Weight for body copy.
    pub fn body_weight(self) -> FontWeight {
        FontWeight::NORMAL
    }

    /// Weight for titles.
    pub fn title_weight(self) -> FontWeight {
        FontWeight::SEMIBOLD
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
    }
}

/// Text for a session failure, kept free of secrets because the domain type is.
pub fn session_failure_text(failure: &SessionFailure) -> String {
    failure.to_string()
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
}

#[cfg(test)]
mod tests {
    use super::{
        StatusMark, Tone, contrast,
        light::{TABLE, TERMINAL},
        light_value, palette,
    };

    /// Minimum contrast for body text (WCAG AA).
    const BODY: f64 = 4.5;
    /// Minimum contrast for status dots, marks, focus rings and inactive controls.
    const GRAPHIC: f64 = 3.0;

    /// The light value of a dark token.
    fn light(token: u32) -> u32 {
        light_value(token)
    }

    /// Every colour token the shell paints with, in one list.
    ///
    /// The light palette is a table keyed by the dark value, so two tokens may never
    /// share one: a duplicate would silently resolve both of them to one light value.
    fn tokens() -> Vec<(&'static str, u32)> {
        let mut tokens = vec![
            ("canvas", palette::CANVAS),
            ("surface", palette::SURFACE),
            ("surface raised", palette::SURFACE_RAISED),
            ("surface hover", palette::SURFACE_HOVER),
            ("surface selected", palette::SURFACE_SELECTED),
            ("surface pressed", palette::SURFACE_PRESSED),
            ("border", palette::BORDER),
            ("border strong", palette::BORDER_STRONG),
            ("segment track", palette::SEGMENT_TRACK),
            ("segment thumb", palette::SEGMENT_THUMB),
            ("text primary", palette::TEXT_PRIMARY),
            ("text secondary", palette::TEXT_SECONDARY),
            ("text muted", palette::TEXT_MUTED),
            ("accent", palette::ACCENT),
            ("accent hover", palette::ACCENT_HOVER),
            ("accent pressed", palette::ACCENT_PRESSED),
            ("on accent", palette::ON_ACCENT),
            ("accent tint", palette::ACCENT_TINT),
            ("accent on tint", palette::ACCENT_ON_TINT),
            ("positive", palette::POSITIVE),
            ("warning", palette::WARNING),
            ("danger", palette::DANGER),
            ("planned", palette::PLANNED),
            ("positive tint", palette::POSITIVE_TINT),
            ("warning tint", palette::WARNING_TINT),
            ("danger tint", palette::DANGER_TINT),
            ("focus ring", palette::FOCUS_RING),
            ("disabled surface", palette::DISABLED_SURFACE),
            ("disabled text", palette::DISABLED_TEXT),
            ("loading track", palette::LOADING_TRACK),
            ("loading fill", palette::LOADING_FILL),
            ("scrim", palette::SCRIM),
            ("brand end", palette::BRAND_END),
        ];
        // The first avatar pair is the accent tint itself, already listed above.
        for (tint, text) in palette::AVATARS.iter().skip(1) {
            tokens.push(("avatar tint", *tint));
            tokens.push(("avatar text", *text));
        }
        for token in palette::TAB_ACCENTS {
            tokens.push(("tab accent", token));
        }
        tokens
    }

    /// The surfaces a control or a label can be drawn on, in both palettes.
    fn surfaces() -> Vec<(&'static str, u32, u32)> {
        [
            ("canvas", palette::CANVAS),
            ("surface", palette::SURFACE),
            ("raised", palette::SURFACE_RAISED),
            ("selected", palette::SURFACE_SELECTED),
            ("pressed", palette::SURFACE_PRESSED),
        ]
        .into_iter()
        .map(|(name, token)| (name, token, light(token)))
        .collect()
    }

    #[test]
    fn no_two_tokens_share_a_value() {
        let tokens = tokens();
        for (index, (name, token)) in tokens.iter().enumerate() {
            for (other_name, other) in tokens.iter().skip(index + 1) {
                assert_ne!(
                    token, other,
                    "{name} and {other_name} are both {token:06x}, so the light table \
                     cannot tell them apart"
                );
            }
        }
        assert_eq!(
            TABLE.len(),
            tokens.len(),
            "every token has exactly one light value"
        );
    }

    #[test]
    fn every_token_has_a_light_value_of_its_own() {
        for (name, token) in tokens() {
            assert_ne!(
                light(token),
                token,
                "{name} ({token:06x}) resolves to itself in the light palette"
            );
        }
    }

    #[test]
    fn the_new_tokens_reach_their_contrast_floor_in_both_palettes() {
        for (surface_name, surface, light_surface) in surfaces() {
            let ring = palette::FOCUS_RING;
            assert!(
                contrast::ratio(ring, surface) >= GRAPHIC,
                "the dark focus ring on {surface_name} is {:.2}:1",
                contrast::ratio(ring, surface)
            );
            assert!(
                contrast::ratio(light(ring), light_surface) >= GRAPHIC,
                "the light focus ring on {surface_name} is {:.2}:1",
                contrast::ratio(light(ring), light_surface)
            );
            // Muted is included: hints, counts and the status bar also sit on a
            // pressed row, so "muted" is not an exemption from the body floor.
            for text in [
                palette::TEXT_PRIMARY,
                palette::TEXT_SECONDARY,
                palette::TEXT_MUTED,
            ] {
                assert!(
                    contrast::ratio(text, surface) >= BODY,
                    "body text on the dark {surface_name} is {:.2}:1",
                    contrast::ratio(text, surface)
                );
                assert!(
                    contrast::ratio(light(text), light_surface) >= BODY,
                    "body text on the light {surface_name} is {:.2}:1",
                    contrast::ratio(light(text), light_surface)
                );
            }
        }

        // An inactive control is still read: its text keeps the graphic floor on its
        // own fill, in both palettes.
        for (text, surface, palette_name) in [
            (palette::DISABLED_TEXT, palette::DISABLED_SURFACE, "dark"),
            (
                light(palette::DISABLED_TEXT),
                light(palette::DISABLED_SURFACE),
                "light",
            ),
        ] {
            let ratio = contrast::ratio(text, surface);
            assert!(
                ratio >= GRAPHIC,
                "{palette_name} disabled text is {ratio:.2}:1"
            );
        }

        // The indicator is a graphic, not text: 3:1 against its own track is enough.
        for (fill, track, palette_name) in [
            (palette::LOADING_FILL, palette::LOADING_TRACK, "dark"),
            (
                light(palette::LOADING_FILL),
                light(palette::LOADING_TRACK),
                "light",
            ),
        ] {
            let ratio = contrast::ratio(fill, track);
            assert!(
                ratio >= GRAPHIC,
                "{palette_name} loading indicator is {ratio:.2}:1"
            );
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
        let scrim = palette::SCRIM;
        for (name, scrim, canvas) in [
            ("dark", scrim, palette::CANVAS),
            ("light", light(scrim), light(palette::CANVAS)),
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
        for (name, token) in [
            ("canvas", palette::CANVAS),
            ("surface", palette::SURFACE),
            ("surface raised", palette::SURFACE_RAISED),
            ("surface hover", palette::SURFACE_HOVER),
            ("surface selected", palette::SURFACE_SELECTED),
            ("surface pressed", palette::SURFACE_PRESSED),
            ("border", palette::BORDER),
            ("border strong", palette::BORDER_STRONG),
            ("text primary", palette::TEXT_PRIMARY),
            ("text secondary", palette::TEXT_SECONDARY),
            ("text muted", palette::TEXT_MUTED),
            ("disabled surface", palette::DISABLED_SURFACE),
            ("disabled text", palette::DISABLED_TEXT),
            ("loading track", palette::LOADING_TRACK),
        ] {
            for (palette_name, value) in [("dark", token), ("light", light(token))] {
                let [_, r, g, b] = value.to_be_bytes();
                let spread = u8::max(u8::max(r, g), b) - u8::min(u8::min(r, g), b);
                assert!(
                    spread <= 14,
                    "{palette_name} {name} is {value:06x}, a {spread}-step hue in the chrome"
                );
            }
        }

        // The one identity colour is cyan-blue: no violet or magenta stops are left in
        // the regular interface (R3).
        for (name, token) in [
            ("accent", palette::ACCENT),
            ("accent hover", palette::ACCENT_HOVER),
            ("brand end", palette::BRAND_END),
        ] {
            for (palette_name, value) in [("dark", token), ("light", light(token))] {
                let [_, r, g, b] = value.to_be_bytes();
                assert!(
                    g > r && b > r,
                    "{palette_name} {name} is {value:06x}, which is not a cyan-blue"
                );
                assert!(
                    b.abs_diff(g) < 0x50,
                    "{palette_name} {name} is {value:06x}, which leans purple"
                );
            }
        }
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
    fn the_light_terminal_keeps_a_cyan_cursor() {
        let cursor = TERMINAL
            .iter()
            .find(|(from, _)| *from == 0x8fa0ff)
            .map(|(_, to)| *to)
            .expect("the cursor has a light value");
        let [_, r, g, b] = cursor.to_be_bytes();
        assert!(
            g > r && b > r,
            "the light cursor {cursor:06x} is not the accent"
        );
    }
}
