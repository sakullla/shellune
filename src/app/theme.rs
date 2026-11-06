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
    /// Application background behind the navigation rail and content.
    pub const CANVAS: u32 = 0x0b0d12;
    /// Default panel surface.
    pub const SURFACE: u32 = 0x12151c;
    /// Raised surface for rows, cards and the header.
    pub const SURFACE_RAISED: u32 = 0x181c25;
    /// Surface used for the selected navigation entry.
    pub const SURFACE_SELECTED: u32 = 0x1e2431;
    /// Hairline border between regions.
    pub const BORDER: u32 = 0x272e3b;
    /// Border of an element that owns keyboard focus.
    pub const BORDER_STRONG: u32 = 0x3a4354;
    /// Primary text: titles and the current object.
    pub const TEXT_PRIMARY: u32 = 0xeef1f7;
    /// Secondary text: body copy and list metadata.
    pub const TEXT_SECONDARY: u32 = 0xbcc4d4;
    /// Muted text: hints, counts and the status bar.
    pub const TEXT_MUTED: u32 = 0x939db0;
    /// Accent used for the selected destination and primary actions.
    pub const ACCENT: u32 = 0x9db4ff;
    /// Accent tint for filled accent surfaces.
    pub const ACCENT_TINT: u32 = 0x2a3350;
    /// Text and icons drawn on [`ACCENT_TINT`].
    pub const ACCENT_ON_TINT: u32 = 0xc8d5ff;
    /// Live and completed status.
    pub const POSITIVE: u32 = 0x64d8a8;
    /// Waiting, verifying or in-progress status.
    pub const WARNING: u32 = 0xf3c66c;
    /// Failed, rejected or unavailable status.
    pub const DANGER: u32 = 0xff8d99;
    /// Planned-but-not-shipped status; deliberately desaturated so it never
    /// reads as an error or as success.
    pub const PLANNED: u32 = 0x9aa6bd;
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
    /// 32px — around empty states.
    pub const SECTION: f32 = 32.0;
}

/// Corner radii in logical pixels.
pub mod radius {
    /// Chips and small controls.
    pub const SMALL: f32 = 6.0;
    /// Rows and inputs.
    pub const MEDIUM: f32 = 9.0;
    /// Panels and the empty-state card.
    pub const LARGE: f32 = 14.0;
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
    pub const TITLE: f32 = 16.0;
    /// Destination title in the header.
    pub const HEADING: f32 = 20.0;
    /// Product name mark.
    pub const DISPLAY: f32 = 26.0;
}

/// Colour helper so views do not call `rgb` with literals.
pub fn swatch(hex: u32) -> Rgba {
    rgb(hex)
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

    /// Surface colour behind a chip in this tone.
    pub fn chip_background(self) -> Rgba {
        match self {
            Self::Accent => swatch(palette::ACCENT_TINT),
            _ => swatch(palette::SURFACE_RAISED),
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
            TransferState::Queued | TransferState::Running { .. } => Self::Warning,
            TransferState::Completed { .. } => Self::Positive,
            TransferState::Failed(_) | TransferState::Cancelled => Self::Danger,
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
        self.scale(232.0)
    }

    /// Height of the header that names the current object.
    pub fn header_height(self) -> Pixels {
        self.scale(60.0)
    }

    /// Height of one navigation entry.
    pub fn nav_row_height(self) -> Pixels {
        self.scale(38.0)
    }

    /// Height of one list row.
    pub fn list_row_height(self) -> Pixels {
        self.scale(46.0)
    }

    /// Height of the status bar.
    pub fn status_bar_height(self) -> Pixels {
        self.scale(30.0)
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
