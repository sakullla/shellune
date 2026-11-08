//! Design tokens for the Shellune shell (R12, R13).
//!
//! The values here are the single source of truth for the palette, spacing,
//! corner radii, type scale and zoom behaviour described in
//! `docs/design/02-shell-and-design-system.md`. Views reference tokens instead of literal
//! colours or sizes, so a visual review only has to change this module.
//!
//! Two properties are checked by tests rather than trusted:
//!
//! * every text and status colour reaches the WCAG contrast floor against the
//!   surface it is drawn on ([`contrast`]);
//! * layout metrics shrink and grow with [`Zoom`] instead of being hard-coded
//!   per view.

use gpui::{FontWeight, Pixels, Rgba, px, rgb};
use shellune_core::{SessionFailure, SessionKind, SessionState, TransferDirection, TransferState};

/// Raw colour tokens as 24-bit sRGB values.
///
/// The palette is a cool, low-chroma dark theme: a near-black canvas, one raised
/// surface per nesting level and a single moon-blue accent for the current
/// object. Status colours are the only saturated hues, which keeps a busy
/// session list readable.
pub mod palette {
    /// Application background: the content area and the terminal itself.
    pub const CANVAS: u32 = 0x0b0c10;
    /// Chrome surface: the navigation rail, the tab strip and the status bar.
    pub const SURFACE: u32 = 0x101116;
    /// Raised surface for cards, rows and inputs.
    pub const SURFACE_RAISED: u32 = 0x16181e;
    /// Surface under the pointer, one step above [`SURFACE_RAISED`].
    pub const SURFACE_HOVER: u32 = 0x1c1f27;
    /// Surface used for the selected navigation entry, tab or choice.
    pub const SURFACE_SELECTED: u32 = 0x23262f;
    /// Hairline border between regions.
    pub const BORDER: u32 = 0x1f222a;
    /// Border of controls and of an element under the pointer.
    pub const BORDER_STRONG: u32 = 0x2e323d;
    /// Primary text: titles and the current object.
    pub const TEXT_PRIMARY: u32 = 0xeceef3;
    /// Secondary text: body copy and list metadata.
    pub const TEXT_SECONDARY: u32 = 0xb3b8c6;
    /// Muted text: hints, counts and the status bar.
    pub const TEXT_MUTED: u32 = 0x8b92a3;
    /// Accent used for the selected destination and primary actions.
    pub const ACCENT: u32 = 0x8fa0ff;
    /// Accent under the pointer, for a solid primary button.
    pub const ACCENT_HOVER: u32 = 0xa9b6ff;
    /// Text drawn on a solid [`ACCENT`] fill.
    pub const ON_ACCENT: u32 = 0x0a0d22;
    /// Accent tint for filled accent surfaces.
    pub const ACCENT_TINT: u32 = 0x1e2340;
    /// Text and icons drawn on [`ACCENT_TINT`].
    pub const ACCENT_ON_TINT: u32 = 0xc9d0ff;
    /// Dark tint behind a positive chip or banner.
    pub const POSITIVE_TINT: u32 = 0x12241d;
    /// Dark tint behind a warning chip or banner.
    pub const WARNING_TINT: u32 = 0x282114;
    /// Dark tint behind a danger chip, banner or destructive button.
    pub const DANGER_TINT: u32 = 0x2b181d;
    /// Live and completed status.
    pub const POSITIVE: u32 = 0x5fd4a0;
    /// Waiting, verifying or in-progress status.
    pub const WARNING: u32 = 0xf0c36d;
    /// Failed, rejected or unavailable status.
    pub const DANGER: u32 = 0xff8c97;
    /// Planned-but-not-shipped status; deliberately desaturated so it never
    /// reads as an error or as success.
    pub const PLANNED: u32 = 0xa2a9ba;
    /// Host avatar pairs (tint, text): iris, violet, teal, amber, rose, sky.
    pub const AVATARS: [(u32, u32); 6] = [
        (0x1e2340, 0xc9d0ff),
        (0x2a1f40, 0xd9c6ff),
        (0x13292a, 0x8fe3d6),
        (0x2b2416, 0xf2d08f),
        (0x2e1a26, 0xffb8d2),
        (0x162536, 0x9fd3ff),
    ];
    /// Second stop of the brand gradient, a violet beside the accent.
    pub const BRAND_END: u32 = 0xb48cff;
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
    /// Product name mark.
    pub const DISPLAY: f32 = 15.0;
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

thread_local! {
    /// The palette the window is being drawn in; per thread, so tests that each run a
    /// window on their own thread never see another test's choice.
    static APPEARANCE: std::cell::Cell<Appearance> = const { std::cell::Cell::new(Appearance::Dark) };
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
        (dark::SURFACE, 0xf3f4f7),
        (dark::SURFACE_RAISED, 0xffffff),
        (dark::SURFACE_HOVER, 0xe9ebf0),
        (dark::SURFACE_SELECTED, 0xe3e6ed),
        (dark::BORDER, 0xe2e4ea),
        (dark::BORDER_STRONG, 0xc9cdd6),
        (dark::TEXT_PRIMARY, 0x1b1d24),
        (dark::TEXT_SECONDARY, 0x454a57),
        (dark::TEXT_MUTED, 0x5f6574),
        (dark::ACCENT, 0x4653cf),
        (dark::ACCENT_HOVER, 0x3a46bd),
        (dark::ON_ACCENT, 0xffffff),
        (dark::ACCENT_TINT, 0xe5e8ff),
        (dark::ACCENT_ON_TINT, 0x2f3ba8),
        (dark::POSITIVE_TINT, 0xe1f4ea),
        (dark::WARNING_TINT, 0xfaefd8),
        (dark::DANGER_TINT, 0xfce6e9),
        (dark::POSITIVE, 0x17734a),
        (dark::WARNING, 0x815400),
        (dark::DANGER, 0xbb2c45),
        (dark::PLANNED, 0x5f6574),
        (dark::BRAND_END, 0x8a57ec),
        // Host avatars after the accent pair they share their first entry with.
        (dark::AVATARS[1].0, 0xefe6ff),
        (dark::AVATARS[1].1, 0x6234b8),
        (dark::AVATARS[2].0, 0xdcf2ee),
        (dark::AVATARS[2].1, 0x0f6a5f),
        (dark::AVATARS[3].0, 0xf9edd2),
        (dark::AVATARS[3].1, 0x7a5000),
        (dark::AVATARS[4].0, 0xfce4ee),
        (dark::AVATARS[4].1, 0xa1285f),
        (dark::AVATARS[5].0, 0xdfeefc),
        (dark::AVATARS[5].1, 0x1b5f9e),
    ];

    /// The terminal's default colours and its 16 ANSI colours, light.
    ///
    /// Keyed by the dark values of `shellune_terminal::palette`; a colour a program
    /// set itself (256-colour or true colour) is drawn as it asked.
    pub const TERMINAL: &[(u32, u32)] = &[
        (0xe6e9f0, 0x1f2329), // foreground
        (0x0b0c10, 0xfcfcfd), // background
        (0x8fa0ff, 0x4653cf), // cursor
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
    match appearance() {
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

impl Tone {
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

    /// Width of the sidebar when it is collapsed to its icons.
    pub fn collapsed_rail_width(self) -> Pixels {
        self.scale(56.0)
    }

    /// Height of the header that names the current object.
    pub fn header_height(self) -> Pixels {
        self.scale(52.0)
    }

    /// Height of one navigation entry.
    pub fn nav_row_height(self) -> Pixels {
        self.scale(34.0)
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

    /// Height of one session tab.
    pub fn tab_height(self) -> Pixels {
        self.scale(32.0)
    }

    /// Widest a reading column of cards and rows grows to.
    ///
    /// Lists and forms stop widening here so a maximised window does not stretch a
    /// host row across a whole monitor; the terminal ignores it and fills the panel.
    pub fn content_max_width(self) -> Pixels {
        self.scale(880.0)
    }

    /// Gap between panels.
    pub fn panel_gap(self) -> Pixels {
        self.scale(spacing::LOOSE)
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

    /// Product name size.
    pub fn display_size(self) -> Pixels {
        self.scale(type_scale::DISPLAY)
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

/// Short label for a transfer direction.
pub fn transfer_direction_label(direction: TransferDirection) -> &'static str {
    direction.label()
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
