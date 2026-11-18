//! Reusable view pieces of the Shellune shell (R12, R13).
//!
//! These helpers take tokens and copy, and return a styled [`gpui::Div`]; they own
//! appearance only. Interaction (click, focus, actions) is added by the caller, so
//! the same chip or row can be used in a list, a header or a confirmation.
//!
//! One table decides how a control looks: [`ControlFace`] gives the fill, border,
//! text, border width and leading mark of a control in each [`ControlState`]. The
//! components below paint their resting face from it and register the hovered face
//! as the pointer style, so a button, an input, a list row and a menu item cannot
//! drift apart. Nothing here invents a colour; every value is a token from
//! [`crate::app::theme`], which is what keeps the two palettes in step.

use gpui::prelude::*;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Div, ElementId, Pixels, Rgba, SharedString,
    StyledText, Window, div, px,
};

use crate::app::icons::{self, Icon};
use crate::app::theme::{
    self, Elevated, Elevation, Enter, Metrics, StatusMark, Token, Tone, palette, radius, spacing,
};

/// The visual weight of a button.
///
/// One screen has at most one [`Variant::Primary`]; everything else steps down, so
/// the eye finds the next action without reading every label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// The one action the screen is about: a solid accent fill.
    Primary,
    /// An ordinary command: the panel fill with a strong hairline border.
    Secondary,
    /// A quiet command that only shows its surface under the pointer.
    Ghost,
    /// An action that removes or replaces something.
    Danger,
    /// A square button that holds only an icon: quiet like [`Variant::Ghost`] until it is
    /// pointed at, and always named by a tooltip.
    Icon,
}

impl Variant {
    /// The five weights, in the order the catalogue shows them.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub const ALL: [Self; 5] = [
        Self::Primary,
        Self::Secondary,
        Self::Ghost,
        Self::Danger,
        Self::Icon,
    ];
}

/// How tall a button is: compact 28 in toolbars and rows, regular 32, large 36 for the
/// one call to action of an empty page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonSize {
    /// 28: toolbars, list rows and banners.
    Compact,
    /// 32: forms, dialogs and pages.
    Regular,
    /// 36: the call to action of an empty page or a welcome.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    Large,
}

impl ButtonSize {
    /// The three sizes, smallest first.
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub const ALL: [Self; 3] = [Self::Compact, Self::Regular, Self::Large];

    /// The height of a button of this size.
    #[must_use]
    pub fn height(self, metrics: Metrics) -> Pixels {
        match self {
            Self::Compact => metrics.compact_control_height(),
            Self::Regular => metrics.control_height(),
            Self::Large => metrics.large_control_height(),
        }
    }

    /// The padding either side of the label.
    fn padding(self, metrics: Metrics) -> Pixels {
        metrics.scale(match self {
            Self::Compact => spacing::TIGHT + spacing::HALF,
            Self::Regular => spacing::SNUG,
            Self::Large => spacing::BASE,
        })
    }

    /// The label size: a compact button sets its word as a caption.
    fn text(self, metrics: Metrics) -> Pixels {
        match self {
            Self::Compact => metrics.caption_size(),
            Self::Regular | Self::Large => metrics.label_size(),
        }
    }
}

/// What the user is doing to a control, or what the control is doing on its own.
///
/// The set is closed on purpose: a page picks one of these instead of writing its own
/// shade, so "hovered" means the same thing in a button, a row and a menu item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum ControlState {
    /// Nothing points at the control and it is not doing anything.
    Resting,
    /// The pointer is on it.
    Hovered,
    /// It is being pressed.
    Pressed,
    /// It is the chosen one - the current tab, the picked menu value.
    Selected,
    /// The keyboard is on it.
    Focused,
    /// It holds or submits something that was rejected.
    Invalid,
    /// It cannot be used right now.
    Disabled,
    /// It is waiting for work it started.
    Loading,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ControlState {
    /// Every state, in the order the documentation lists them.
    pub const ALL: [Self; 8] = [
        Self::Resting,
        Self::Hovered,
        Self::Pressed,
        Self::Selected,
        Self::Focused,
        Self::Invalid,
        Self::Disabled,
        Self::Loading,
    ];
}

/// Everything one control paints in one state, as palette tokens.
///
/// The border width and the mark are part of the face because a state is never told
/// by colour alone: a focused control is thicker, a chosen one carries a leading bar,
/// a disabled one loses its pointer and its text drops to the disabled token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlFace {
    fill: Option<Token>,
    border: Option<Token>,
    border_width: f32,
    text: Token,
    mark: Option<Token>,
    busy: bool,
    pressed: bool,
}

impl ControlFace {
    /// A face with a hairline border and nothing else.
    fn new(fill: Option<Token>, border: Option<Token>, text: Token) -> Self {
        Self {
            fill,
            border,
            border_width: 1.0,
            text,
            mark: None,
            busy: false,
            pressed: false,
        }
    }

    /// The fill behind the control, or nothing to draw one.
    #[must_use]
    pub fn fill_token(self) -> Option<Token> {
        self.fill
    }

    /// The border around the control, or nothing to draw one.
    #[must_use]
    pub fn border_token(self) -> Option<Token> {
        self.border
    }

    /// The label and icon colour.
    #[must_use]
    pub fn text_token(self) -> Token {
        self.text
    }

    /// The leading bar that marks the chosen control, when this state has one.
    #[must_use]
    pub fn mark_token(self) -> Option<Token> {
        self.mark
    }

    /// The border width in logical pixels; 2 marks a focused or rejected control.
    #[must_use]
    pub fn border_width_px(self) -> f32 {
        self.border_width
    }

    /// Whether this face is waiting on work and must say so with an indicator.
    #[must_use]
    pub fn is_busy(self) -> bool {
        self.busy
    }

    /// Whether the pointer is holding this control down.
    ///
    /// A pressed control also thickens its bottom edge, so a press is felt as a change
    /// of edge and not only as a change of tint.
    #[must_use]
    pub fn is_pressed(self) -> bool {
        self.pressed
    }

    /// The fill in the palette being drawn.
    #[must_use]
    pub fn fill_color(self) -> Rgba {
        self.fill_token().map_or_else(theme::clear, theme::swatch)
    }

    /// The border in the palette being drawn.
    #[must_use]
    pub fn border_color(self) -> Rgba {
        self.border_token().map_or_else(theme::clear, theme::swatch)
    }

    /// The label colour in the palette being drawn.
    #[must_use]
    pub fn text_color(self) -> Rgba {
        theme::swatch(self.text)
    }

    fn pressed(self) -> Self {
        Self {
            pressed: true,
            ..self
        }
    }

    fn with_mark(self, mark: Token) -> Self {
        Self {
            mark: Some(mark),
            ..self
        }
    }

    fn busy(self) -> Self {
        Self { busy: true, ..self }
    }

    /// The same face with a secondary ring around it, which is how focus and a
    /// rejected value are drawn: the fill stays as it was, the edge doubles.
    fn ringed(self, ring: Token) -> Self {
        Self {
            border: Some(ring),
            border_width: 2.0,
            ..self
        }
    }
}

/// The face of a [`Variant`] in one [`ControlState`].
///
/// The table is the single answer to "what does this button look like now": every
/// state is one of these rows, so a focused soft button and a focused danger button
/// differ only in the tokens, not in the way the state is expressed.
#[must_use]
pub fn button_face(variant: Variant, state: ControlState) -> ControlFace {
    let resting = match variant {
        Variant::Primary => ControlFace::new(
            Some(palette::ACCENT),
            Some(palette::ACCENT),
            palette::ON_ACCENT,
        ),
        Variant::Secondary => ControlFace::new(
            Some(palette::SURFACE),
            Some(palette::BORDER_STRONG),
            palette::TEXT_PRIMARY,
        ),
        Variant::Ghost | Variant::Icon => ControlFace::new(None, None, palette::TEXT_SECONDARY),
        Variant::Danger => ControlFace::new(
            Some(palette::DANGER_TINT),
            Some(palette::DANGER_TINT),
            palette::DANGER,
        ),
    };
    let hovered = match variant {
        Variant::Primary => ControlFace::new(
            Some(palette::ACCENT_HOVER),
            Some(palette::ACCENT_HOVER),
            palette::ON_ACCENT,
        ),
        Variant::Danger => resting.with_border(palette::DANGER),
        Variant::Secondary => resting
            .with_fill(palette::SURFACE_HOVER)
            .with_border(palette::BORDER_STRONG),
        Variant::Ghost | Variant::Icon => resting
            .with_fill(palette::SURFACE_HOVER)
            .with_text(palette::TEXT_PRIMARY),
    };
    match state {
        ControlState::Resting => resting,
        ControlState::Hovered => hovered,
        ControlState::Pressed => match variant {
            // The primary button sinks onto a darker accent; the quiet ones take the
            // pressed fill. Every press also thickens the bottom edge, which is the
            // press the pointer can see.
            Variant::Primary => ControlFace::new(
                Some(palette::ACCENT_PRESSED),
                Some(palette::ACCENT_PRESSED),
                palette::ON_ACCENT,
            )
            .pressed(),
            Variant::Ghost | Variant::Icon | Variant::Secondary => resting
                .with_fill(palette::SURFACE_PRESSED)
                .with_text(palette::TEXT_PRIMARY)
                .pressed(),
            Variant::Danger => hovered.pressed(),
        },
        ControlState::Selected => match variant {
            Variant::Primary => ControlFace::new(
                Some(palette::ACCENT_PRESSED),
                Some(palette::ACCENT_PRESSED),
                palette::ON_ACCENT,
            )
            .with_mark(palette::ON_ACCENT),
            Variant::Secondary => resting
                .with_fill(palette::SURFACE_SELECTED)
                .with_border(palette::BORDER_STRONG)
                .with_mark(palette::ACCENT),
            Variant::Ghost | Variant::Icon => resting
                .with_fill(palette::SURFACE_SELECTED)
                .with_text(palette::TEXT_PRIMARY)
                .with_mark(palette::ACCENT),
            Variant::Danger => resting
                .with_border(palette::DANGER)
                .with_mark(palette::DANGER),
        },
        ControlState::Focused => match variant {
            // The focus ring has to be seen against the page and against the fill. On a
            // solid accent button the accent ring would disappear into it, so the edge
            // takes the label's ink instead - the same focus, in the colour that
            // contrasts there.
            Variant::Primary => resting.ringed(palette::ON_ACCENT),
            Variant::Secondary | Variant::Ghost | Variant::Icon | Variant::Danger => {
                resting.ringed(palette::FOCUS_RING)
            }
        },
        ControlState::Invalid => match variant {
            // A rejected form makes its primary action unusable, so the action itself
            // drops its accent fill and says why in the danger tone.
            Variant::Primary => ControlFace::new(
                Some(palette::DANGER_TINT),
                Some(palette::DANGER),
                palette::DANGER,
            )
            .ringed(palette::DANGER),
            Variant::Secondary | Variant::Ghost | Variant::Icon | Variant::Danger => {
                resting.ringed(palette::DANGER)
            }
        },
        ControlState::Disabled => ControlFace::new(
            Some(palette::DISABLED_SURFACE),
            Some(palette::BORDER),
            palette::DISABLED_TEXT,
        ),
        ControlState::Loading => resting.busy(),
    }
}

impl ControlFace {
    fn with_fill(self, fill: Token) -> Self {
        Self {
            fill: Some(fill),
            ..self
        }
    }

    fn with_border(self, border: Token) -> Self {
        Self {
            border: Some(border),
            ..self
        }
    }

    fn with_text(self, text: Token) -> Self {
        Self { text, ..self }
    }
}

/// Applies a face to a control: fill, hairline border, border width and label colour.
///
/// The border is always there, transparent when the face has none, so a control does
/// not shift by a pixel when it becomes focused or selected.
fn painted(base: Div, face: ControlFace) -> Div {
    let base = base
        .border_color(face.border_color())
        .text_color(face.text_color());
    let base = if face.border_width_px() >= 2.0 {
        base.border_2()
    } else {
        base.border_1()
    };
    // A pressed control thickens its bottom edge, so the press is a change of shape as
    // well as of tint.
    let base = if face.is_pressed() {
        base.border_b_2()
    } else {
        base
    };
    if face.fill_token().is_some() {
        base.bg(face.fill_color())
    } else {
        base
    }
}

/// The hover style of a face, for the controls that answer the pointer themselves.
fn hover_style(face: ControlFace) -> impl FnOnce(gpui::StyleRefinement) -> gpui::StyleRefinement {
    move |style| {
        style
            .bg(face.fill_color())
            .border_color(face.border_color())
            .text_color(face.text_color())
    }
}

/// The width of the leading bar that marks the chosen item.
const MARK_BAR_WIDTH: f32 = 2.0;
/// The stroke of a hollow status mark and the height of the dash.
const MARK_STROKE: f32 = 1.5;
/// The rounding of the small marks: the bar, the squares and the dash.
const MARK_CORNER: f32 = 1.0;

/// The 2px leading bar that says "this one is chosen" without relying on colour.
fn mark_bar(token: Token, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .w(px(MARK_BAR_WIDTH))
        .h(metrics.small_icon_size())
        .rounded(px(MARK_CORNER))
        .bg(theme::swatch(token))
}

/// The silhouette of a tone, drawn in its colour.
///
/// Shape, then word, then colour: a filled disc is live, a hollow ring is waiting, a
/// solid square is a failure, a hollow square is not shipped, a bar is the current
/// object and a dash is nothing in particular. Every shape keeps one square footprint,
/// so a chip is the same size whatever state it is in.
pub fn status_mark(tone: Tone, size: Pixels) -> Div {
    let color = tone.text();
    let shape = match tone.mark() {
        StatusMark::Disc => div().w(size).h(size).rounded_full().bg(color),
        StatusMark::Ring => div()
            .w(size)
            .h(size)
            .rounded_full()
            .border(px(MARK_STROKE))
            .border_color(color),
        StatusMark::Square => div()
            .w(size * 0.86)
            .h(size * 0.86)
            .rounded(px(MARK_CORNER))
            .bg(color),
        StatusMark::Box => div()
            .w(size * 0.86)
            .h(size * 0.86)
            .rounded(px(MARK_CORNER))
            .border(px(MARK_STROKE))
            .border_color(color),
        StatusMark::Bar => div()
            .w(size * 0.34)
            .h(size)
            .rounded(px(MARK_CORNER))
            .bg(color),
        StatusMark::Dash => div()
            .w(size)
            .h(px(MARK_STROKE))
            .rounded(px(MARK_CORNER))
            .bg(color),
        StatusMark::Target => div()
            .flex()
            .items_center()
            .justify_center()
            .w(size)
            .h(size)
            .rounded_full()
            .border(px(MARK_STROKE))
            .border_color(color)
            .child(div().size(size * 0.3).rounded_full().bg(color)),
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(size)
        .h(size)
        .child(shape)
}

/// A small status indicator in the tone's own silhouette.
pub fn status_dot(tone: Tone, metrics: Metrics) -> Div {
    status_mark(tone, metrics.scale(7.0))
}

/// A named status dot that breathes while the state it marks is still on its way, such
/// as a connection being made (R2); `breathing` says whether it is.
///
/// The breath repeats only while full motion is allowed. With reduced motion the dot
/// is drawn still and at full strength: its silhouette and tone already say the state,
/// and the word is in the tooltip of whatever carries the dot. `selector` names the dot,
/// and a breathing dot also carries `<selector>.pulse` so a test can tell the two apart.
pub fn pulsing_status_dot(
    tone: Tone,
    metrics: Metrics,
    breathing: bool,
    id: impl Into<gpui::ElementId>,
    selector: String,
) -> Div {
    let dot = status_dot(tone, metrics);
    let dot = if breathing && theme::motion_allowed() {
        let pulse = format!("{selector}.pulse");
        dot.debug_selector(move || pulse.clone())
            .with_animation(
                id,
                Animation::new(theme::Motion::PULSE)
                    .repeat()
                    .with_easing(gpui::pulsating_between(0.35, 1.0)),
                |dot, delta| dot.opacity(delta),
            )
            .into_any_element()
    } else {
        dot.into_any_element()
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .debug_selector(move || selector.clone())
        .child(dot)
}

/// A compact label that carries a state's word, mark and tone.
///
/// It is a hairline block, not a pill: the mark's shape and the word carry the state,
/// and the tone only agrees with them.
pub fn chip(label: impl Into<SharedString>, tone: Tone, metrics: Metrics) -> Div {
    chip_frame(tone, metrics).flex_none().child(label.into())
}

/// A [`chip`] that gives way to the box it is placed in: its words end in an ellipsis
/// rather than running past the box, and the mark stays whole.
pub fn chip_within(label: impl Into<SharedString>, tone: Tone, metrics: Metrics) -> Div {
    chip_frame(tone, metrics)
        .flex_initial()
        .min_w_0()
        .overflow_hidden()
        .child(div().min_w_0().truncate().child(label.into()))
}

/// The tint, edge and mark of a chip, without its words.
fn chip_frame(tone: Tone, metrics: Metrics) -> Div {
    // A soft block: the tint and the mark carry the tone, so the edge stays faint. A
    // neutral chip is a plain word - its dash mark read as a minus sign in front of it.
    div()
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::NARROW))
        .h(metrics.scale(20.0))
        .px(metrics.scale(spacing::TIGHT))
        .rounded(px(radius::SMALL))
        .bg(tone.chip_background())
        .border_1()
        .border_color(theme::faded(tone.text(), 0.16))
        .text_size(metrics.caption_size())
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(tone.text())
        .when(tone != Tone::Neutral, |chip| {
            chip.child(status_mark(tone, metrics.scale(spacing::NARROW)))
        })
}

/// A quiet label for a grouping such as a host's group: no mark, because it is not a
/// state.
pub fn tag(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(metrics.scale(20.0))
        .px(metrics.scale(spacing::NARROW))
        .rounded(px(radius::SMALL))
        .bg(theme::swatch(palette::SURFACE_SELECTED))
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .min_w_0()
        // At rest a tag is as wide as its words; capped by its parent (a list row's tag
        // column), it ends in an ellipsis instead of spilling over.
        .child(div().min_w_0().truncate().child(line_text(label)))
}

/// The caption size a keycap's label is set in: a step under the caption, so a
/// shortcut beside a word reads as a hint and not as a second word.
fn kbd_text_size(metrics: Metrics) -> Pixels {
    metrics.caption_size() - metrics.scale(1.0)
}

/// A keycap that names a shortcut, such as `Ctrl+K` beside a command (ADR-2).
pub fn kbd(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(metrics.scale(18.0))
        .px(metrics.scale(spacing::HAIRLINE + 1.0))
        .rounded(px(radius::SMALL))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_b_2()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .text_size(kbd_text_size(metrics))
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .child(label.into())
}

/// A count in a small block, for the sidebar and section headings.
pub fn badge(count: usize, tone: Tone, metrics: Metrics) -> Div {
    let (background, text) = match tone {
        Tone::Accent => (palette::ACCENT_TINT, palette::ACCENT_ON_TINT),
        _ => (palette::SURFACE_SELECTED, palette::TEXT_SECONDARY),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(metrics.scale(18.0))
        .h(metrics.scale(18.0))
        .px(metrics.scale(spacing::NARROW))
        .rounded(px(radius::SMALL))
        .font_weight(gpui::FontWeight::MEDIUM)
        .bg(theme::swatch(background))
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(text))
        .child(SharedString::from(count.to_string()))
}

/// A tiny label that names what kind of object something is, such as "SSH".
pub fn kind_badge(label: impl Into<SharedString>, remote: bool, metrics: Metrics) -> Div {
    let (background, text) = if remote {
        (palette::ACCENT_TINT, palette::ACCENT_ON_TINT)
    } else {
        (palette::SURFACE_SELECTED, palette::TEXT_SECONDARY)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(metrics.scale(16.0))
        .px(metrics.scale(spacing::HAIRLINE + 1.0))
        .rounded(px(radius::SMALL))
        .bg(theme::swatch(background))
        .text_size(kbd_text_size(metrics))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::swatch(text))
        .child(label.into())
}

/// The frame every labelled button shares: its height, padding, corner and label size,
/// painted with `face`.
fn button_frame(size: ButtonSize, face: ControlFace, metrics: Metrics) -> Div {
    painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(metrics.scale(spacing::NARROW))
            .h(size.height(metrics))
            .px(size.padding(metrics))
            .rounded(px(radius::MEDIUM))
            .text_size(size.text(metrics))
            .font_weight(gpui::FontWeight::MEDIUM),
        face,
    )
}

/// A command button in one of the [`Variant`] weights, at the regular height.
///
/// It paints its resting face and answers the pointer with its hovered face; the states
/// the caller owns are reached through [`button_with_state`].
pub fn button(label: impl Into<SharedString>, variant: Variant, metrics: Metrics) -> Div {
    button_sized(label, variant, ButtonSize::Regular, metrics)
}

/// [`button`] at one of the three [`ButtonSize`]s.
pub fn button_sized(
    label: impl Into<SharedString>,
    variant: Variant,
    size: ButtonSize,
    metrics: Metrics,
) -> Div {
    let base = button_frame(size, button_face(variant, ControlState::Resting), metrics)
        .cursor_pointer()
        .child(label.into());
    lifted(base, variant).hover(hover_style(button_face(variant, ControlState::Hovered)))
}

/// Only the primary button rests on the page with a soft shadow; every other weight
/// lies flat and is told by its edge.
fn lifted(base: Div, variant: Variant) -> Div {
    match variant {
        Variant::Primary => base.elevation(Elevation::Raised),
        Variant::Secondary | Variant::Ghost | Variant::Danger | Variant::Icon => base,
    }
}

/// [`button`] in a state the caller owns: focused, pressed, disabled or waiting.
///
/// The caller knows whether the keyboard is on the control and whether the work it
/// started is still running, so it passes the state in instead of the component
/// guessing from a colour.
#[cfg_attr(not(test), allow(dead_code))]
pub fn button_with_state(
    label: impl Into<SharedString>,
    variant: Variant,
    state: ControlState,
    metrics: Metrics,
) -> Div {
    let face = button_face(variant, state);
    let base = button_frame(ButtonSize::Regular, face, metrics)
        .when(state != ControlState::Disabled, |el| el.cursor_pointer())
        .child(label.into());
    let base = match face.mark_token() {
        Some(token) => base.child(mark_bar(token, metrics)),
        None => base,
    };
    if face.is_busy() {
        base.child(loading_indicator(metrics))
    } else {
        base
    }
}

/// A button that leads with an icon.
///
/// The icon takes the button's text colour, so every variant and hover state works
/// unchanged.
pub fn icon_label_button(
    icon: Icon,
    label: impl Into<SharedString>,
    variant: Variant,
    metrics: Metrics,
) -> Div {
    let color = variant_text(variant);
    let hover_text = theme::swatch(button_face(variant, ControlState::Hovered).text_token());
    let drawing = icons::icon(icon, metrics.small_icon_size(), color);
    let drawing = if matches!(variant, Variant::Ghost | Variant::Icon) {
        drawing.group_hover(ICON_GROUP, move |style| style.text_color(hover_text))
    } else {
        drawing
    };
    let base = button_frame(
        ButtonSize::Regular,
        button_face(variant, ControlState::Resting),
        metrics,
    )
    .group(ICON_GROUP)
    .pl(metrics.scale(spacing::SNUG - spacing::HALF))
    .cursor_pointer()
    .child(drawing)
    .child(label.into());
    lifted(base, variant).hover(hover_style(button_face(variant, ControlState::Hovered)))
}

/// A square button holding one icon, quiet until pointed at: [`Variant::Icon`].
pub fn icon_only_button(icon: Icon, metrics: Metrics) -> Div {
    let hovered = button_face(Variant::Icon, ControlState::Hovered);
    icon_only_button_hover(icon, metrics, hovered.fill_color(), hovered.text_color())
}

/// [`icon_only_button`] for a destructive action: it warns in the danger colour under
/// the pointer, before the click.
pub fn danger_icon_button(icon: Icon, metrics: Metrics) -> Div {
    icon_only_button_hover(
        icon,
        metrics,
        theme::swatch(Tone::Danger.tint()),
        theme::swatch(palette::DANGER),
    )
}

/// [`icon_only_button`] for a switch that stays on: while `on` it sits on the accent
/// tint and draws its icon in the tint's own text colour, so the state is read from
/// the button itself and not only from its tooltip.
pub fn toggle_icon_button(icon: Icon, on: bool, metrics: Metrics) -> Div {
    if !on {
        return icon_only_button(icon, metrics);
    }
    let tint = theme::swatch(palette::ACCENT_TINT);
    let ink = theme::swatch(palette::ACCENT_ON_TINT);
    let size = metrics.hit_target();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(size)
        .h(size)
        .rounded(px(radius::MEDIUM))
        .bg(tint)
        .text_color(ink)
        .cursor_pointer()
        .child(icons::icon(icon, metrics.small_icon_size(), ink))
}

fn icon_only_button_hover(icon: Icon, metrics: Metrics, hover: Rgba, hover_text: Rgba) -> Div {
    let size = metrics.hit_target();
    let resting = variant_text(Variant::Icon);
    div()
        .group(ICON_GROUP)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(size)
        .h(size)
        .rounded(px(radius::MEDIUM))
        .text_color(resting)
        .cursor_pointer()
        .hover(move |style| style.bg(hover).text_color(hover_text))
        .child(
            icons::icon(icon, metrics.small_icon_size(), resting)
                .group_hover(ICON_GROUP, move |style| style.text_color(hover_text)),
        )
}

/// The hover group an icon button's drawing follows.
pub const ICON_GROUP: &str = "icon-button";

/// The text colour a variant draws its label in, for icons that sit beside it.
fn variant_text(variant: Variant) -> Rgba {
    theme::swatch(button_face(variant, ControlState::Resting).text_token())
}

/// A compact button for toolbars, rows and banners: [`ButtonSize::Compact`], 28 high.
pub fn compact_button(label: impl Into<SharedString>, variant: Variant, metrics: Metrics) -> Div {
    button_sized(label, variant, ButtonSize::Compact, metrics)
}

/// A square button that holds a short glyph such as "A+" or "×".
pub fn icon_button(glyph: impl Into<SharedString>, metrics: Metrics) -> Div {
    let size = metrics.hit_target();
    painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .min_w(size)
            .h(size)
            .px(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.caption_size())
            .cursor_pointer()
            .child(glyph.into()),
        button_face(Variant::Icon, ControlState::Resting),
    )
    .hover(hover_style(button_face(
        Variant::Icon,
        ControlState::Hovered,
    )))
}

/// One option of a segmented choice: a shell, a view or a sign-in method.
pub fn segment(label: impl Into<SharedString>, selected: bool, metrics: Metrics) -> Div {
    let base = div()
        .flex()
        .flex_none()
        .items_center()
        .h(metrics.compact_control_height())
        .px(metrics.scale(spacing::TIGHT + spacing::HALF))
        .rounded(px(radius::SMALL))
        .text_size(metrics.label_size())
        .cursor_pointer()
        .child(label.into());
    if selected {
        base.bg(theme::swatch(palette::SEGMENT_THUMB))
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .font_weight(gpui::FontWeight::MEDIUM)
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .elevation(Elevation::Raised)
    } else {
        let hover = theme::swatch(palette::TEXT_PRIMARY);
        base.text_color(theme::swatch(palette::TEXT_MUTED))
            .border_1()
            .border_color(theme::clear())
            .hover(move |style| style.text_color(hover))
    }
}

/// The track a row of [`segment`]s sits in.
///
/// The track shrinks to its column and wraps. `flex_none` would keep it at the sum of its
/// segments: the settings scroller's horizontal overflow is visible, so that sum becomes
/// the scroller's minimum width, the row grows past the window, and `flex_wrap` never
/// sees a definite line to wrap on. The test font draws CJK narrower than the DirectWrite
/// font the window uses, so the same track stays inside the test window and still paints
/// past the real window's right edge (R4, 720×480 at 200% text).
pub fn segmented(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_shrink()
        .flex_wrap()
        .items_center()
        .min_w_0()
        .max_w_full()
        .gap(metrics.scale(spacing::HALF))
        .p(metrics.scale(spacing::HALF))
        .rounded(px(radius::MEDIUM))
        .bg(theme::swatch(palette::SEGMENT_TRACK))
        .border_1()
        .border_color(theme::faded(theme::swatch(palette::BORDER), 0.7))
}

/// Em widths reserved so a label is at least as wide as a UI font, not the test font.
///
/// Wide glyphs (CJK, fullwidth forms) take a full em. Other glyphs take a little over
/// half an em, which covers Segoe-like Latin without exceeding the settings row: a floor
/// wider than the row beats `max_w_full` and the control paints past the card.
const WIDE_EM: f32 = 1.0;
const NARROW_EM: f32 = 0.55;

fn is_wide_glyph(ch: char) -> bool {
    matches!(
        ch,
        '\u{1100}'..='\u{115F}'
            | '\u{2329}'..='\u{232A}'
            | '\u{2E80}'..='\u{A4CF}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE10}'..='\u{FE19}'
            | '\u{FE30}'..='\u{FE6F}'
            | '\u{FF00}'..='\u{FF60}'
            | '\u{FFE0}'..='\u{FFE6}'
            | '\u{1F300}'..='\u{1F64F}'
            | '\u{1F900}'..='\u{1F9FF}'
            | '\u{20000}'..='\u{3FFFD}'
    )
}

/// Width to reserve for `text` at `text_size`, in logical pixels.
pub(crate) fn reserved_text_width(text: &str, text_size: Pixels) -> Pixels {
    let units = text
        .chars()
        .map(|ch| {
            if is_wide_glyph(ch) {
                WIDE_EM
            } else {
                NARROW_EM
            }
        })
        .sum::<f32>();
    text_size * units
}

/// Inner width of a settings row at the minimum window for this zoom.
///
/// The page keeps `scale(32)` on each side and the row keeps `scale(16)`. A control
/// floor wider than what remains cannot sit inside the card.
pub(crate) fn settings_row_content_cap(metrics: Metrics) -> Pixels {
    let window = px(crate::app::MIN_WINDOW_SIZE.0);
    let page_pad = metrics.scale(spacing::LOOSE + 8.0) * 2.0;
    let row_pad = metrics.scale(spacing::BASE) * 2.0;
    window - page_pad - row_pad
}

/// Border-box floor for one [`segment`], capped so it still fits in the track.
pub(crate) fn segment_reserved_width(label: &str, metrics: Metrics) -> Pixels {
    let text = reserved_text_width(label, metrics.label_size());
    let padding = metrics.scale(spacing::TIGHT + spacing::HALF) * 2.0;
    let border = px(2.0);
    let track_pad = metrics.scale(spacing::HALF) * 2.0;
    let track_border = px(2.0);
    let cap = settings_row_content_cap(metrics) - track_pad - track_border;
    (text + padding + border).min(cap)
}

/// Border-box floor for a [`compact_button`] label.
pub(crate) fn compact_label_reserved_width(label: &str, metrics: Metrics) -> Pixels {
    let text = reserved_text_width(label, metrics.caption_size());
    let padding = metrics.scale(spacing::TIGHT + 2.0) * 2.0;
    let border = px(2.0);
    (text + padding + border).min(settings_row_content_cap(metrics))
}

/// Border-box floor for a [`kbd`].
pub(crate) fn kbd_reserved_width(label: &str, metrics: Metrics) -> Pixels {
    let text = reserved_text_width(label, kbd_text_size(metrics));
    let padding = metrics.scale(spacing::HAIRLINE + 1.0) * 2.0;
    let border = px(2.0);
    (text + padding + border).min(settings_row_content_cap(metrics))
}

/// A short label above a group of controls.
pub fn section_label(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    // An eyebrow, not a heading: small and quiet, so the content under it leads.
    div()
        .flex_none()
        .text_size(metrics.caption_size())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// A tinted block that carries a notice, a question or a failure in one tone.
///
/// The left edge carries the tone as a solid bar, so a warning is told apart from a
/// failure at a glance and the text keeps its full contrast on the tint.
pub fn banner(tone: Tone, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .gap(metrics.scale(spacing::HAIRLINE))
        .pl(metrics.scale(spacing::SNUG + 2.0))
        .pr(metrics.scale(spacing::TIGHT))
        .py(metrics.scale(spacing::TIGHT + 2.0))
        .rounded(px(radius::MEDIUM))
        .bg(tone.chip_background())
        .border_1()
        .border_l(metrics.scale(3.0))
        .border_color(theme::faded(tone.text(), 0.35))
        .text_size(metrics.label_size())
}

/// A finished notification: the banner's shape, with a mark, a title and one line.
///
/// It is the same block the inline notices use, so a toast and a form's error do not
/// look like two different products.
#[cfg_attr(not(test), allow(dead_code))]
pub fn notice(
    tone: Tone,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    metrics: Metrics,
) -> Div {
    banner(tone, metrics)
        .gap(metrics.scale(spacing::HAIRLINE + 1.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .child(status_mark(tone, metrics.scale(7.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(metrics.label_size())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(title.into()),
                ),
        )
        .child(
            div()
                .pl(metrics.scale(spacing::TIGHT + 7.0))
                .whitespace_normal()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .child(body.into()),
        )
}

/// A label beside the control it names, for the forms.
pub fn field_row(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::SNUG))
        .min_w_0()
        .child(
            div()
                .flex_none()
                .w(metrics.scale(104.0))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(label.into()),
        )
}

/// The face of a text field in one state.
///
/// A field is a surface, not a button: it keeps its raised fill and says focus or
/// rejection with a doubled border in the focus or danger colour. Those two are
/// different tokens as well, so the difference survives a monochrome screenshot.
#[must_use]
pub fn field_face(state: ControlState) -> ControlFace {
    let resting = ControlFace::new(
        Some(palette::SURFACE),
        Some(palette::BORDER),
        palette::TEXT_PRIMARY,
    );
    match state {
        ControlState::Hovered => resting.with_border(palette::BORDER_STRONG),
        ControlState::Pressed => resting.with_border(palette::BORDER_STRONG).pressed(),
        ControlState::Selected => resting.with_mark(palette::ACCENT),
        ControlState::Focused => resting.ringed(palette::FOCUS_RING),
        ControlState::Invalid => resting.ringed(palette::DANGER),
        ControlState::Disabled => ControlFace::new(
            Some(palette::DISABLED_SURFACE),
            Some(palette::BORDER),
            palette::DISABLED_TEXT,
        ),
        ControlState::Loading => resting.busy(),
        ControlState::Resting => resting,
    }
}

/// The frame a text field is drawn inside: fill, hairline border, focus ring.
///
/// The caller keeps its own [`crate::app::text_field::TextField`] and puts it in here, so
/// the frame and the editing behaviour can be reviewed apart.
#[cfg_attr(not(test), allow(dead_code))]
pub fn field_frame(state: ControlState, metrics: Metrics) -> Div {
    let face = field_face(state);
    let base = painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .min_w_0()
            .h(metrics.control_height())
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::TIGHT + 2.0))
            .rounded(px(radius::MEDIUM)),
        face,
    );
    if face.is_busy() {
        base.child(loading_indicator(metrics))
    } else {
        base
    }
}

/// The card used for every non-populated section state.
///
/// Empty, loading and failed sections share one skeleton — tone, title,
/// body and an optional next step — so the difference between them is what the
/// copy says and which tone carries it, never the shape of the card.
pub fn state_card(
    tone: Tone,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    next_step: Option<SharedString>,
    metrics: Metrics,
) -> Div {
    let emblem_icon = match tone {
        Tone::Danger => None,
        Tone::Warning => None,
        _ => Some(Icon::Layers),
    };
    state_card_with_icon(tone, emblem_icon, title, body, next_step, metrics)
}

/// [`state_card`] with a chosen emblem: the destination's own icon reads better than
/// a dot when a page is simply empty.
pub fn state_card_with_icon(
    tone: Tone,
    emblem_icon: Option<Icon>,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    next_step: Option<SharedString>,
    metrics: Metrics,
) -> Div {
    let emblem = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(metrics.scale(44.0))
        .h(metrics.scale(44.0))
        .rounded(px(radius::LARGE))
        .bg(tone.chip_background())
        .border_1()
        .border_color(theme::faded(tone.text(), 0.28))
        .child(match emblem_icon {
            Some(icon) => icons::icon(icon, metrics.scale(20.0), tone.text()).into_any_element(),
            None => attention_dot(tone, metrics),
        });

    // Each line of words goes through [`capped_text`]: a column with `w_full()` and a
    // `max_w` asks its wrapping text for its height at the card's whole width and then
    // paints it at the cap, so a sentence that fits across the card but not inside the
    // cap kept a one-line box, painted two lines and ran into the padding and the step
    // under it (R1, R5).
    let cap = metrics.scale(480.0);
    let mut text = div()
        .flex()
        .flex_col()
        .items_center()
        .w_full()
        .min_w_0()
        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
        .child(capped_text(
            cap,
            div()
                .whitespace_normal()
                .text_size(metrics.title_size() + px(1.0))
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .font_weight(metrics.title_weight())
                .text_center()
                .debug_selector(|| "state.card.title".to_owned())
                .child(title.into()),
        ))
        .child(capped_text(
            cap,
            div()
                .whitespace_normal()
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .font_weight(metrics.body_weight())
                .text_center()
                .debug_selector(|| "state.card.body".to_owned())
                .child(body.into()),
        ));
    if let Some(step) = next_step {
        text = text.child(
            capped_text(
                cap,
                div()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(match tone {
                        Tone::Neutral | Tone::Planned => theme::swatch(palette::TEXT_MUTED),
                        tone => tone.text(),
                    })
                    .text_center()
                    .debug_selector(|| "state.card.step".to_owned())
                    .child(step),
            )
            .mt(metrics.scale(spacing::HAIRLINE)),
        );
    }

    div()
        .debug_selector(|| "state.card".to_owned())
        .flex()
        .flex_col()
        .items_center()
        .gap(metrics.scale(spacing::BASE))
        .w_full()
        .px(metrics.scale(spacing::LOOSE))
        .py(metrics.scale(spacing::LOOSE + 16.0))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .elevation(Elevation::Raised)
        .child(emblem)
        .child(text)
}

/// The mark on a waiting card.
///
/// Full motion fades it in once. Reduced motion draws it at full strength immediately,
/// and the card's words stay either way. The fade is not a loop: a load that takes
/// time is still told by the warning copy, not by something that keeps moving.
fn attention_dot(tone: Tone, metrics: Metrics) -> AnyElement {
    let dot = status_mark(tone, metrics.scale(10.0));
    if tone == Tone::Warning && theme::motion_allowed() {
        dot.debug_selector(|| "state.motion.pulse".to_owned())
            .with_animation(
                "state.motion",
                Animation::new(theme::Motion::BASE).with_easing(theme::ease_out()),
                |dot, delta| dot.opacity(0.35 + 0.65 * delta),
            )
            .into_any_element()
    } else if tone == Tone::Warning {
        dot.debug_selector(|| "state.motion.still".to_owned())
            .into_any_element()
    } else {
        dot.into_any_element()
    }
}

/// The face of a list row in one state.
#[must_use]
pub fn row_face(state: ControlState) -> ControlFace {
    let resting = ControlFace::new(
        Some(palette::SURFACE),
        Some(palette::BORDER),
        palette::TEXT_PRIMARY,
    );
    match state {
        ControlState::Hovered => resting
            .with_fill(palette::SURFACE_HOVER)
            .with_border(palette::BORDER_STRONG),
        ControlState::Pressed => resting
            .with_fill(palette::SURFACE_PRESSED)
            .with_border(palette::BORDER_STRONG)
            .pressed(),
        ControlState::Selected => resting
            .with_fill(palette::SURFACE_SELECTED)
            .with_border(palette::BORDER_STRONG)
            .with_mark(palette::ACCENT),
        ControlState::Focused => resting.ringed(palette::FOCUS_RING),
        ControlState::Invalid => resting.ringed(palette::DANGER),
        ControlState::Disabled => ControlFace::new(
            Some(palette::DISABLED_SURFACE),
            Some(palette::BORDER),
            palette::DISABLED_TEXT,
        ),
        ControlState::Loading => resting.busy(),
        ControlState::Resting => resting,
    }
}

/// A list row frame shared by hosts, sessions and transfers.
pub fn row(metrics: Metrics) -> Div {
    painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::SNUG))
            .w_full()
            .min_h(metrics.list_row_height() + metrics.scale(4.0))
            .px(metrics.scale(spacing::BASE - 2.0))
            .py(metrics.scale(spacing::TIGHT + 2.0))
            .rounded(px(radius::LARGE)),
        row_face(ControlState::Resting),
    )
    .hover(hover_style(row_face(ControlState::Hovered)))
}

/// [`row`] in a state the caller owns: the chosen row, the focused row, or the one whose
/// transfer failed.
#[cfg_attr(not(test), allow(dead_code))]
pub fn row_with_state(state: ControlState, metrics: Metrics) -> Div {
    let face = row_face(state);
    let base = painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::SNUG))
            .w_full()
            .min_h(metrics.list_row_height() + metrics.scale(4.0))
            .px(metrics.scale(spacing::BASE - 2.0))
            .py(metrics.scale(spacing::TIGHT + 2.0))
            .rounded(px(radius::LARGE)),
        face,
    );
    let base = match face.mark_token() {
        Some(token) => base.child(mark_bar(token, metrics)),
        None => base,
    };
    if face.is_busy() {
        base.child(loading_indicator(metrics))
    } else {
        base
    }
}

/// The face of one menu entry in one state.
#[must_use]
pub fn menu_item_face(state: ControlState) -> ControlFace {
    let resting = ControlFace::new(None, None, palette::TEXT_SECONDARY);
    match state {
        ControlState::Hovered => resting
            .with_fill(palette::SURFACE_HOVER)
            .with_text(palette::TEXT_PRIMARY),
        ControlState::Pressed => resting
            .with_fill(palette::SURFACE_PRESSED)
            .with_text(palette::TEXT_PRIMARY)
            .pressed(),
        ControlState::Selected => ControlFace::new(
            Some(palette::ACCENT_TINT),
            Some(palette::ACCENT_TINT),
            palette::ACCENT_ON_TINT,
        )
        .with_mark(palette::ACCENT),
        ControlState::Focused => resting.ringed(palette::FOCUS_RING),
        ControlState::Invalid => resting.ringed(palette::DANGER),
        ControlState::Disabled => ControlFace::new(None, None, palette::DISABLED_TEXT),
        ControlState::Loading => resting.busy(),
        ControlState::Resting => resting,
    }
}

/// The floating card a menu is drawn in: the raised layer, a strong edge and a mid
/// shadow, so it reads as over the page and not part of it.
///
/// Every context and drop-down menu uses this card, so a session row's menu and a tab's
/// menu are the same object.
pub fn menu_card(selector: &'static str, metrics: Metrics) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_col()
        .min_w(metrics.scale(200.0))
        .gap(metrics.scale(1.0))
        .p(metrics.scale(spacing::HAIRLINE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .elevation(Elevation::Overlay)
}

/// One command of a [`menu_card`]: an optional icon and a label.
///
/// An unavailable command stays in its place, muted and without a pointer, so the menu
/// does not change shape with the selection; a destructive one is drawn in the danger
/// tone before it is pressed.
pub fn menu_entry(
    label: impl Into<SharedString>,
    selector: &'static str,
    icon: Option<Icon>,
    enabled: bool,
    danger: bool,
    metrics: Metrics,
) -> gpui::Stateful<Div> {
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let color = if !enabled {
        theme::swatch(palette::TEXT_MUTED)
    } else if danger {
        theme::swatch(palette::DANGER)
    } else {
        theme::swatch(palette::TEXT_PRIMARY)
    };
    let danger_hover = theme::swatch(palette::DANGER_TINT);
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .h(metrics.scale(30.0))
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .rounded(px(radius::MEDIUM - 2.0))
        .text_size(metrics.label_size())
        .text_color(color)
        .when(enabled, |item| {
            let hover = if danger { danger_hover } else { hover };
            item.cursor_pointer().hover(move |style| style.bg(hover))
        })
        .children(icon.map(|icon| icons::icon(icon, metrics.small_icon_size(), color)))
        // The label takes the room, so a shortcut appended after it sits at the far end
        // without pushing on the gap between the icon and the word.
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
}

/// The shortcut at the far end of a [`menu_entry`]: the binding in force, quiet, so
/// the command's own word is what the eye reads first.
///
/// The caller names it when a test reads it; an action with no binding draws none.
pub fn menu_shortcut(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .pl(metrics.scale(spacing::BASE))
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// The small heading over one group of a [`menu_card`]'s entries.
pub fn menu_section(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .pt(metrics.scale(spacing::HAIRLINE + 2.0))
        .pb(metrics.scale(2.0))
        .text_size(metrics.caption_size())
        .font_weight(metrics.medium_weight())
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// A [`menu_card`] with its entrance: it fades in and rises the few pixels into the
/// place it is anchored at, once per opening (`id` names the opening), and is simply
/// there under reduced motion.
///
/// The rise is a margin on the anchored card, not a move of the anchor, so the menu
/// still opens where it was asked to and its entries are pressable from the first
/// frame.
pub fn menu_enter(
    menu: impl IntoElement + Styled + 'static,
    id: impl Into<ElementId>,
) -> AnyElement {
    theme::enter(menu, id, Enter::Rise)
}

/// A bar of controls that floats over a picture or a page: the raised layer with a
/// strong edge and a mid shadow, like a menu, so it reads as above what it controls.
///
/// The remote desktop's zoom and full-screen controls are drawn on it. The caller
/// places it and adds the controls; a press on the bar stays on the bar.
pub fn floating_bar(metrics: Metrics) -> Div {
    div()
        .occlude()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::HALF))
        .p(metrics.scale(spacing::HAIRLINE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .elevation(Elevation::Overlay)
}

/// The hairline between groups of [`menu_entry`]s.
pub fn menu_divider(metrics: Metrics) -> Div {
    div()
        .flex_none()
        .h(px(HAIRLINE_WIDTH))
        .mx(metrics.scale(spacing::HAIRLINE))
        .my(metrics.scale(spacing::HAIRLINE - 1.0))
        .bg(theme::swatch(palette::BORDER))
}

/// The panel a choice menu is drawn in: a [`menu_card`] without a selector.
#[cfg_attr(not(test), allow(dead_code))]
pub fn menu_panel(metrics: Metrics) -> Div {
    menu_card("menu.panel", metrics).flex_none().min_w_0()
}

/// One value of a [`menu_panel`], drawn from the [`menu_item_face`] table.
#[cfg_attr(not(test), allow(dead_code))]
pub fn menu_item(label: impl Into<SharedString>, selected: bool, metrics: Metrics) -> Div {
    let state = if selected {
        ControlState::Selected
    } else {
        ControlState::Resting
    };
    let face = menu_item_face(state);
    let base = painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::SNUG))
            .h(metrics.control_height())
            .px(metrics.scale(spacing::TIGHT + 2.0))
            .rounded(px(radius::SMALL))
            .cursor_pointer()
            .child(label.into()),
        face,
    );
    if selected {
        // The chosen entry keeps its own face under the pointer: the row has to keep
        // saying which value is set while the pointer travels through the menu.
        base
    } else {
        base.hover(hover_style(menu_item_face(ControlState::Hovered)))
    }
}

/// The face of a dialog's own surface.
#[must_use]
pub fn dialog_face() -> ControlFace {
    ControlFace::new(
        Some(palette::OVERLAY),
        Some(palette::BORDER_STRONG),
        palette::TEXT_PRIMARY,
    )
}

/// The surface every dialog-level layer is drawn on - a [`modal`]'s card and the
/// command palette: the overlay layer with a strong edge, the dialog radius and the
/// highest elevation, its content clipped to its corners.
///
/// It takes no presses through it: what is under a dialog is not reached through it.
pub fn dialog_surface() -> Div {
    painted(
        div()
            .flex()
            .flex_col()
            .rounded(px(radius::XLARGE))
            .elevation(Elevation::Overlay)
            .overflow_hidden()
            .occlude(),
        dialog_face(),
    )
}

/// The centred panel a dialog is drawn on: the overlay layer at the highest elevation.
#[cfg_attr(not(test), allow(dead_code))]
pub fn dialog_panel(metrics: Metrics) -> Div {
    painted(
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .gap(metrics.scale(spacing::BASE))
            .p(metrics.scale(spacing::LOOSE))
            .rounded(px(radius::XLARGE))
            .elevation(Elevation::Overlay),
        dialog_face(),
    )
}

/// The title block at the top of a [`dialog_panel`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn dialog_header(
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(metrics.scale(spacing::SNUG))
        .w_full()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap(metrics.scale(spacing::HALF))
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(metrics.title_size() + px(1.0))
                        .font_weight(metrics.title_weight())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(title.into()),
                )
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(subtitle.into()),
                ),
        )
}

/// A value read as an endpoint: an address, a port, a path, a shortcut.
///
/// These are read character by character, so they are set in the terminal's own
/// monospace face while the labels around them stay in the interface face. It is one of
/// the two places the shell uses two families; the other is the terminal grid.
#[cfg_attr(not(test), allow(dead_code))]
pub fn endpoint(value: impl Into<SharedString>, metrics: Metrics) -> Div {
    let value: SharedString = value.into();
    copy_on_click(
        div()
            .flex_none()
            .min_w_0()
            .truncate()
            .font_family(crate::app::terminal::monospace_family())
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY)),
        value.to_string(),
    )
    .child(value)
}

/// Makes a value one click away from the clipboard.
///
/// The shell draws its text itself, and a drawn line cannot be dragged over and
/// selected the way a document's can; the values a person copies elsewhere -
/// addresses, paths, fingerprints, the reason a connection failed - therefore copy
/// whole on a click, and the window says so with a toast ([`crate::app::actions::ShowCopied`]).
pub fn copy_on_click(base: Div, text: impl Into<String>) -> Div {
    let text: String = text.into();
    let hover = theme::swatch(palette::TEXT_PRIMARY);
    base.cursor_pointer()
        .hover(move |style| style.text_color(hover))
        .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            window.dispatch_action(Box::new(crate::app::actions::ShowCopied), cx);
        })
}

/// A short in-flight bar: the work is running, and the word beside it says what.
///
/// The bar never carries the meaning on its own - a loading button also says what it is
/// doing - and with motion reduced it is drawn still instead of moving.
#[cfg_attr(not(test), allow(dead_code))]
pub fn loading_indicator(metrics: Metrics) -> AnyElement {
    let bar = div()
        .flex_none()
        .w(metrics.scale(36.0))
        .h(px(2.0))
        .rounded_full()
        .bg(theme::swatch(palette::LOADING_TRACK))
        .child(
            div()
                .w(metrics.scale(14.0))
                .h_full()
                .rounded_full()
                .bg(theme::swatch(palette::LOADING_FILL)),
        );
    if theme::motion_allowed() {
        bar.debug_selector(|| "state.loading.motion".to_owned())
            .with_animation(
                "state.loading",
                Animation::new(theme::Motion::BASE).with_easing(theme::ease_out()),
                |bar, delta| bar.opacity(0.4 + 0.6 * delta),
            )
            .into_any_element()
    } else {
        bar.debug_selector(|| "state.loading.still".to_owned())
            .into_any_element()
    }
}

/// A label/value pair stacked in a row.
pub fn stacked(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(metrics.scale(spacing::HALF))
        .child(
            div()
                .text_size(metrics.body_size())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .truncate()
                .child(label.into()),
        )
        .child(
            div()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .truncate()
                .child(value.into()),
        )
}

/// A hairline between two groups of controls.
pub fn divider_vertical(metrics: Metrics) -> Div {
    div()
        .flex_none()
        .w(px(HAIRLINE_WIDTH))
        .h(metrics.icon_size())
        .bg(theme::swatch(palette::BORDER))
}

/// The small floating label a control shows under the pointer.
pub struct Tooltip {
    text: SharedString,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // A tooltip is its own view, drawn outside the window's render pass; the
        // window's text size comes from the theme so a 200% window gets a 200% tip.
        let metrics = theme::metrics();
        div()
            .px(metrics.scale(spacing::TIGHT + 1.0))
            .py(metrics.scale(spacing::HAIRLINE + 1.0))
            .rounded(px(radius::SMALL))
            .bg(theme::swatch(palette::RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .elevation(Elevation::Overlay)
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .child(self.text.clone())
    }
}

/// Builds the tooltip callback a stateful element takes.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_window, cx| cx.new(|_cx| Tooltip { text: text.clone() }).into()
}

/// A card: a block of content on the surface layer, resting on the page at a low
/// elevation.
///
/// It is the one container a page groups its content in - a settings section, a home
/// tile, a status panel - so the corner, edge and shadow of every card agree.
pub fn card(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(metrics.scale(spacing::SNUG))
        .p(metrics.scale(spacing::BASE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .elevation(Elevation::Raised)
}

/// The card a session's state is laid over its terminal in: why it ended, or that it is
/// on its way (R3).
///
/// It floats over the grid, so it is the raised layer at the overlay elevation. The
/// emblem tile carries the tone, the title states the state and the optional line says
/// what it means; the caller appends the actions and any disclosure. A press on the card
/// stays on the card: it occludes the grid under it, while the grid around it keeps its
/// pointer.
///
/// The card has no width of its own: it is placed through [`status_card_slot`], which
/// caps it the way [`capped_text`] caps a line. A card that carried `w_full` and a
/// `max_w` in a column was asked for its height at the column's whole width and painted
/// at the cap, so a long diagnostic painted more lines than its box held and ran out of
/// the card (R5).
pub fn status_card(
    tone: Tone,
    emblem: AnyElement,
    title: Div,
    detail: Option<Div>,
    metrics: Metrics,
) -> Div {
    div()
        .occlude()
        .flex()
        .flex_col()
        .flex_none()
        .min_w_0()
        .gap(metrics.scale(spacing::SNUG))
        .p(metrics.scale(spacing::WIDE))
        .rounded(px(radius::XLARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .elevation(Elevation::Overlay)
        .cursor_default()
        .child(
            div()
                .flex()
                .items_start()
                .gap(metrics.scale(spacing::SNUG))
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.large_control_height())
                        .rounded(px(radius::LARGE))
                        .bg(tone.chip_background())
                        .child(emblem),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(metrics.scale(spacing::HALF))
                        .child(
                            title
                                .whitespace_normal()
                                .text_size(metrics.title_size())
                                .font_weight(metrics.title_weight())
                                .text_color(theme::swatch(palette::TEXT_PRIMARY)),
                        )
                        .children(detail.map(|detail| {
                            detail
                                .whitespace_normal()
                                .text_size(metrics.label_size())
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        })),
                ),
        )
}

/// An on/off switch for a setting that takes effect at once.
///
/// The knob's side says the state before its colour does: off sits left on a grey
/// track, on sits right on the accent-hued track. The knob keeps the graphic floor on
/// both tracks, hovered or not, in both palettes. The caller gives it an id and a click;
/// the switch only draws.
#[cfg_attr(not(test), allow(dead_code))]
pub fn switch(on: bool, metrics: Metrics) -> Div {
    let width = metrics.scale(34.0);
    let height = metrics.scale(20.0);
    let inset = metrics.scale(3.0);
    let knob = height - inset * 2.0;
    let track = if on {
        palette::TRACK_ON
    } else {
        palette::TRACK_OFF
    };
    let hover = theme::swatch(if on {
        palette::TRACK_ON_HOVER
    } else {
        palette::TRACK_OFF_HOVER
    });
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(width)
        .h(height)
        .px(inset)
        .rounded_full()
        .bg(theme::swatch(track))
        .cursor_pointer()
        .when(on, |track| track.justify_end())
        .when(!on, |track| track.justify_start())
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .flex_none()
                .size(knob)
                .rounded_full()
                .bg(theme::swatch(palette::KNOB))
                .elevation(Elevation::Raised),
        )
}

/// The hover group a [`checkbox_box`] follows, so the label's row lights the box.
pub const CHECK_GROUP: &str = "checkbox";

/// The box of a checkbox alone: a hollow square that fills with the accent and a tick
/// when checked.
///
/// It is drawn as what it is - a checkbox - rather than borrowing a chevron, which reads
/// as "open this". Under the pointer of its [`CHECK_GROUP`] an empty box takes the
/// accent edge, so the box says it can be picked before it is.
pub fn checkbox_box(checked: bool, metrics: Metrics) -> Div {
    let accent = theme::swatch(palette::ACCENT);
    let boxed = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.scale(16.0))
        .rounded(px(radius::SMALL - 2.0))
        .border_1();
    if checked {
        boxed.bg(accent).border_color(accent).child(icons::icon(
            Icon::Check,
            metrics.scale(12.0),
            theme::swatch(palette::ON_ACCENT),
        ))
    } else {
        boxed
            .bg(theme::swatch(palette::SURFACE))
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .group_hover(CHECK_GROUP, move |style| style.border_color(accent))
    }
}

/// A checkbox with its label, for a choice inside a form.
pub fn checkbox(
    id: &'static str,
    label: impl Into<SharedString>,
    checked: bool,
    metrics: Metrics,
) -> gpui::Stateful<Div> {
    let hover = theme::swatch(palette::TEXT_PRIMARY);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .group(CHECK_GROUP)
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .cursor_pointer()
        .text_size(metrics.label_size())
        .text_color(theme::swatch(if checked {
            palette::TEXT_PRIMARY
        } else {
            palette::TEXT_SECONDARY
        }))
        .hover(move |style| style.text_color(hover))
        .child(checkbox_box(checked, metrics))
        .child(label.into())
}

/// A finished notification: a tone's icon on its tint, a title and an optional line.
///
/// The caller gives it its id, its close button and its place in the stack; the toast
/// is the raised card and its words. It floats over the page, so it is drawn on the
/// raised layer at the mid elevation, like a menu.
pub fn toast(
    tone: Tone,
    icon: Icon,
    title: Div,
    detail: Option<SharedString>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .items_start()
        .gap(metrics.scale(spacing::TIGHT + 2.0))
        .w(metrics.scale(340.0))
        .p(metrics.scale(spacing::SNUG))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .elevation(Elevation::Overlay)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(metrics.scale(24.0))
                .rounded_full()
                .bg(theme::swatch(tone.tint()))
                .child(icons::icon(icon, metrics.small_icon_size(), tone.text())),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(metrics.scale(2.0))
                .child(
                    title
                        .text_size(metrics.label_size())
                        .font_weight(metrics.medium_weight())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY)),
                )
                .children(detail.map(|detail| {
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .line_clamp(2)
                        .child(detail)
                })),
        )
}

/// A wrapping text with a width cap, centred in the row it is given, laid out at the
/// width it is painted at.
///
/// A wrapping text measures its lines once per width it is asked about and paints the
/// lines of the last ask. As a flex item of a column, a text with `w_full()` and a
/// `max_w` was asked for its height at the column's whole width - the cap is not
/// applied there - and then laid out and painted at the capped width: a line that fit
/// across the column but not inside the cap kept a one-line box and painted two, the
/// second over whatever came next (R1). Here the text is an item of a row, where its
/// width is the column's width clamped to the cap before its height is asked for, so
/// the box is as tall as the lines that are painted in it.
///
/// Every centred, capped, wrapping text in a column - an empty state's title and line,
/// a picker's hint - goes through this rather than putting `max_w` on the text itself.
pub fn capped_text(max_width: Pixels, text: Div) -> Div {
    capped(max_width, text)
}

/// Words for a line that ends in an ellipsis where it does not fit: the child of an
/// element with `.truncate()` (and `.min_w_0()` in a row).
///
/// It is the text element itself rather than the bare words so the tests can read back
/// the line as it was shaped and see the "…" that was drawn (R1); in the application it
/// is the same text a string child would give.
pub fn line_text(text: impl Into<SharedString>) -> StyledText {
    let text: SharedString = text.into();
    let element = StyledText::new(text.clone());
    #[cfg(test)]
    drawn::keep(text, element.layout().clone());
    element
}

/// The lines [`line_text`] shaped, as the tests read them back.
#[cfg(test)]
pub mod drawn {
    use std::cell::RefCell;

    use gpui::{SharedString, TextLayout};

    thread_local! {
        static LINES: RefCell<Vec<(SharedString, TextLayout)>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn keep(text: SharedString, layout: TextLayout) {
        LINES.with(|lines| lines.borrow_mut().push((text, layout)));
    }

    /// Forgets the lines of earlier tests on this thread.
    pub fn clear() {
        LINES.with(|lines| lines.borrow_mut().clear());
    }

    /// The line `text` was last shaped into: the words that were drawn, with the "…"
    /// where they were cut.
    pub fn shown(text: &str) -> Option<String> {
        LINES.with(|lines| {
            lines
                .borrow()
                .iter()
                .rev()
                .find(|(words, _)| words.as_ref() == text)
                .map(|(_, layout)| layout.text())
        })
    }
}

/// Any block - a card, a column of words - centred with a width cap and laid out at the
/// width it is painted at, by the rule of [`capped_text`].
pub fn capped(max_width: Pixels, block: Div) -> Div {
    div()
        .flex()
        .flex_none()
        .justify_center()
        .w_full()
        .min_w_0()
        .child(block.w_full().min_w_0().max_w(max_width))
}

/// The widest a [`status_card`] grows, in logical pixels before zoom.
pub const STATUS_CARD_WIDTH: f32 = 440.0;

/// Places a [`status_card`] in the column it is shown in: centred, as wide as the
/// column up to [`STATUS_CARD_WIDTH`], and measured at the width it is painted at.
pub fn status_card_slot(card: Div, metrics: Metrics) -> Div {
    capped(metrics.scale(STATUS_CARD_WIDTH), card)
}

/// An empty page or section: an icon on the accent tint, a title, an optional line and
/// an optional next step.
///
/// "Empty" is a state of its own and says what to do next; it is not the failure card,
/// so it carries no status tone. The caller names it with its own selector when a page
/// has more than one way to be empty.
pub fn empty_state(
    icon: Icon,
    title: impl Into<SharedString>,
    body: Option<SharedString>,
    action: Option<AnyElement>,
    metrics: Metrics,
) -> Div {
    div()
        .debug_selector(|| "empty.state".to_owned())
        .flex()
        .flex_col()
        .items_center()
        .w_full()
        .min_w_0()
        .gap(metrics.scale(spacing::SNUG))
        .px(metrics.scale(spacing::LOOSE))
        .py(metrics.scale(spacing::LOOSE + 8.0))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(metrics.scale(48.0))
                .rounded(px(radius::XLARGE))
                .bg(theme::swatch(palette::ACCENT_TINT))
                .child(icons::icon(
                    icon,
                    metrics.scale(22.0),
                    theme::swatch(palette::ACCENT_ON_TINT),
                )),
        )
        .child(capped_text(
            metrics.scale(420.0),
            div()
                .whitespace_normal()
                .text_center()
                .text_size(metrics.title_size())
                .font_weight(metrics.title_weight())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .debug_selector(|| "empty.state.title".to_owned())
                .child(title.into()),
        ))
        .children(body.map(|body| {
            capped_text(
                metrics.scale(420.0),
                div()
                    .debug_selector(|| "empty.state.body".to_owned())
                    .whitespace_normal()
                    .text_center()
                    .text_size(metrics.body_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(body),
            )
        }))
        .children(action.map(|action| {
            div()
                .debug_selector(|| "empty.state.action".to_owned())
                .mt(metrics.scale(spacing::HAIRLINE))
                .child(action)
        }))
}

/// The pieces of one centred dialog.
///
/// The subtitle is its own piece and is drawn at the top of the scrolling body.
/// Putting that sentence in the fixed header left the body too short to reach at 200%
/// text (R4).
pub struct ModalContent {
    /// The fixed header: icon tile, title, close button.
    pub header: Div,
    /// The sentence under the title, scrolled with the body.
    pub subtitle: Option<Div>,
    /// What the dialog asks or shows.
    pub body: AnyElement,
    /// The fixed row of actions, primary last.
    pub footer: Option<Div>,
}

/// What a press on the dimmed page around a [`modal`] does.
pub type ModalDismiss = Box<dyn Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static>;

/// How one dialog differs from the shared behaviour of [`modal`].
#[derive(Default)]
pub struct ModalBehaviour {
    /// The handle the scrolling body reports to, for a dialog that scrolls a field
    /// into view itself (the host editor).
    pub scroll: Option<gpui::ScrollHandle>,
    /// What a press on the scrim does. `None` keeps the dialog open: a form keeps what
    /// was typed until it is closed on purpose. Only a dialog with nothing to lose - a
    /// shortcut being captured - closes on a press outside.
    pub dismiss: Option<ModalDismiss>,
}

/// The fixed row of actions at the bottom of a [`modal`].
///
/// The actions sit on the right in reading order, the primary one last, and wrap
/// under each other when the dialog is narrower than the row (720x480 at 200%). A
/// hairline separates them from the scrolling body, so the row stays put while the
/// body moves under it.
pub fn modal_footer(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .flex_wrap()
        .gap(metrics.scale(spacing::TIGHT))
        .px(metrics.scale(spacing::LOOSE))
        .py(metrics.scale(spacing::SNUG))
        .border_t_1()
        .border_color(theme::swatch(palette::BORDER))
}

/// The icon tile at the head of a [`modal`]: the accent tint for an ordinary dialog,
/// the tone's tint for a question about something that cannot be undone.
pub fn modal_icon(icon: Icon, tone: Option<Tone>, metrics: Metrics) -> Div {
    let (fill, ink) = match tone {
        Some(tone) => (theme::swatch(tone.tint()), tone.text()),
        None => (
            theme::swatch(palette::ACCENT_TINT),
            theme::swatch(palette::ACCENT_ON_TINT),
        ),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.scale(32.0))
        .rounded(px(radius::MEDIUM))
        .bg(fill)
        .child(icons::icon(icon, metrics.icon_size(), ink))
}

/// A centred dialog over a dimmed page.
///
/// The dialog is the overlay layer at the highest elevation, with the largest panel
/// radius; the body scrolls between a fixed header and footer. The scrim takes every
/// press so nothing behind it can be used, but a press on it does not close the
/// dialog: what was typed is kept until the user closes it on purpose. With motion
/// allowed the dialog fades in and settles into place.
pub fn modal(
    viewport: gpui::Size<Pixels>,
    id: &'static str,
    width: f32,
    content: ModalContent,
    metrics: Metrics,
) -> Div {
    modal_with(
        viewport,
        id,
        width,
        content,
        ModalBehaviour::default(),
        metrics,
    )
}

/// [`modal`] with its own scroll handle or its own answer to a press outside.
pub fn modal_with(
    viewport: gpui::Size<Pixels>,
    id: &'static str,
    width: f32,
    content: ModalContent,
    behaviour: ModalBehaviour,
    metrics: Metrics,
) -> Div {
    let ModalContent {
        header,
        subtitle,
        body,
        footer,
    } = content;
    let ModalBehaviour { scroll, dismiss } = behaviour;
    // The inset stays a small edge gap. Scaling it with the text turned a 200% minimum
    // window into a short dialog, and the failure's next line was drawn past the client
    // edge where it could not be read (R4).
    let margin = metrics.scale(spacing::LOOSE).min(px(16.0));
    let width = metrics.scale(width).min(viewport.width - margin * 2.0);
    let max_height = viewport.height - margin * 2.0;
    let dialog = dialog_surface()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w(width)
        .max_h(max_height)
        .child(header)
        .child(
            div()
                .id(SharedString::from(format!("{id}.body")))
                .debug_selector(move || format!("{id}.body"))
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when_some(scroll, |body, handle| body.track_scroll(&handle))
                .children(subtitle)
                .child(body),
        )
        .children(footer.map(|footer| footer.flex_none()));
    let backdrop = div()
        .id(SharedString::from(format!("{id}.backdrop")))
        .debug_selector(move || format!("{id}.backdrop"))
        .absolute()
        .inset_0()
        .bg(theme::scrim())
        .occlude();
    let backdrop = match dismiss {
        Some(dismiss) => backdrop.on_any_mouse_down(move |event, window, cx| {
            cx.stop_propagation();
            dismiss(event, window, cx);
        }),
        None => backdrop.on_mouse_down(gpui::MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        }),
    };
    div()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w(viewport.width)
        .h(viewport.height)
        .child(backdrop)
        .child(theme::enter(
            dialog,
            SharedString::from(format!("{id}.enter")),
            Enter::Settle,
        ))
}

/// How long the dimmed page takes to clear after a dialog closes.
pub const SCRIM_EXIT: std::time::Duration = theme::Motion::BASE;

/// The dimming a [`modal`] leaves behind as it closes, fading out (R5).
///
/// The dialog itself is gone at once: GPUI rebuilds the tree every frame, so keeping a
/// closed dialog drawn would mean keeping every dialog's state alive after its answer.
/// The dimming carries no state, so it alone eases away. It takes no presses - the
/// page is usable again the moment the dialog is answered - and `key` restarts the
/// fade for each dialog that closes. Under reduced motion there is nothing to draw:
/// the window does not ask for it.
pub fn modal_scrim_exit(viewport: gpui::Size<Pixels>, key: u64) -> AnyElement {
    div()
        .debug_selector(|| "modal.scrim.exit".to_owned())
        .absolute()
        .top_0()
        .left_0()
        .w(viewport.width)
        .h(viewport.height)
        .bg(theme::scrim())
        .with_animation(
            SharedString::from(format!("modal.scrim.exit.{key}")),
            Animation::new(SCRIM_EXIT).with_easing(theme::ease_out()),
            |scrim, delta| scrim.opacity(1.0 - delta),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Component library v2 (ADR-2): search, rows, sidebar, toolbar, banner, progress,
// skeleton
// ---------------------------------------------------------------------------

/// The one search box a page has: a magnifier, the caller's text field, an optional
/// clear button and an optional shortcut hint (ADR-2).
///
/// The field is the caller's own [`crate::app::text_field::TextField`]; the clear
/// button is the caller's too, so it can carry its id and its click. The frame is the
/// [`field_face`] of `state`, so a focused search box rings like every other field.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn search_field(
    field: AnyElement,
    clear: Option<AnyElement>,
    shortcut: Option<SharedString>,
    state: ControlState,
    metrics: Metrics,
) -> Div {
    painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .min_w_0()
            .h(metrics.control_height())
            .gap(metrics.scale(spacing::NARROW))
            .pl(metrics.scale(spacing::TIGHT + spacing::HALF))
            .pr(metrics.scale(spacing::NARROW))
            .rounded(px(radius::MEDIUM)),
        field_face(state),
    )
    .child(icons::icon(
        Icon::Search,
        metrics.small_icon_size(),
        theme::swatch(palette::TEXT_MUTED),
    ))
    .child(
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .child(field),
    )
    .children(clear)
    .children(shortcut.map(|shortcut| kbd(shortcut, metrics)))
}

/// The face of a [`list_row`] in one state.
///
/// A list row is flat: it lies on the page with no fill and no edge, and only the
/// pointer, the selection and the keyboard give it a surface. The chosen row also
/// carries the accent bar, so "chosen" is not told by a shade alone.
#[must_use]
pub fn list_row_face(state: ControlState) -> ControlFace {
    let resting = ControlFace::new(None, None, palette::TEXT_PRIMARY);
    match state {
        ControlState::Resting => resting,
        ControlState::Hovered => resting.with_fill(palette::SURFACE_HOVER),
        ControlState::Pressed => resting.with_fill(palette::SURFACE_PRESSED).pressed(),
        ControlState::Selected => resting
            .with_fill(palette::SURFACE_SELECTED)
            .with_mark(palette::ACCENT),
        ControlState::Focused => resting.ringed(palette::FOCUS_RING),
        ControlState::Invalid => resting.ringed(palette::DANGER),
        ControlState::Disabled => ControlFace::new(
            Some(palette::DISABLED_SURFACE),
            None,
            palette::DISABLED_TEXT,
        ),
        ControlState::Loading => resting.busy(),
    }
}

/// The hover group a [`list_row`]'s revealed actions follow.
pub const ROW_GROUP: &str = "list-row";

/// The least height of a [`list_row`]: two lines of words and their padding.
const LIST_ROW_MIN_HEIGHT: f32 = 44.0;

/// The frame of a [`list_row`] alone, for a row that lays out its own content.
pub fn list_row_frame(state: ControlState, metrics: Metrics) -> Div {
    row_frame(
        state,
        false,
        metrics,
        metrics.scale(LIST_ROW_MIN_HEIGHT),
        metrics.scale(spacing::NARROW),
        metrics.scale(spacing::SNUG),
        metrics.scale(spacing::TIGHT + spacing::HALF),
    )
}

/// The frame of a dense row: a [`list_row_frame`] one compact control high (28), for
/// the long lists of a tool - a file listing, a transfer queue (ADR-7).
///
/// `marked` is the batch's own mark: the row is one of the entries a batch action will
/// cover, so it keeps the accent tint under the pointer as well. The keyboard's row is
/// `ControlState::Selected`, the selected fill with the accent bar, as in every list.
pub fn compact_row_frame(state: ControlState, marked: bool, metrics: Metrics) -> Div {
    row_frame(
        state,
        marked,
        metrics,
        metrics.compact_control_height(),
        px(0.0),
        metrics.scale(spacing::TIGHT),
        metrics.scale(spacing::NARROW),
    )
}

/// The one frame both list rows are drawn in.
fn row_frame(
    state: ControlState,
    marked: bool,
    metrics: Metrics,
    min_height: Pixels,
    padding_y: Pixels,
    gap: Pixels,
    mark_inset: Pixels,
) -> Div {
    let face = list_row_face(state);
    let base = painted(
        div()
            .group(ROW_GROUP)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(gap)
            .w_full()
            .min_w_0()
            .min_h(min_height)
            .px(metrics.scale(spacing::SNUG))
            .py(padding_y)
            .rounded(px(radius::MEDIUM)),
        face,
    );
    let base = match state {
        ControlState::Resting if marked => base.bg(theme::swatch(palette::ACCENT_TINT)),
        ControlState::Resting => base.hover(hover_style(list_row_face(ControlState::Hovered))),
        _ => base,
    };
    match face.mark_token() {
        // The accent bar stands in the row's left padding, so the content does not move
        // when the row is chosen.
        Some(token) => base.child(
            div()
                .absolute()
                .left(px(0.0))
                .top(mark_inset)
                .bottom(mark_inset)
                .w(px(MARK_BAR_WIDTH))
                .rounded(px(MARK_CORNER))
                .bg(theme::swatch(token)),
        ),
        None => base,
    }
}

/// The frame of a question or a short form drawn in the flow of the column it is
/// about (ADR-7): the raised fill and the strong hairline of a floating layer, without
/// lifting off the page, so it reads as "answer this here" rather than as a dialog.
///
/// The caller adds the [`form_heading`], the labelled fields and the buttons as its
/// children, in reading order. A dangerous question keeps a danger edge, so it is not
/// told by the colour of its icon alone.
pub fn inline_form(tone: Tone, metrics: Metrics) -> Div {
    let edge = match tone {
        Tone::Danger | Tone::Warning => theme::faded(tone.text(), 0.45),
        _ => theme::swatch(palette::BORDER_STRONG),
    };
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .min_w_0()
        .gap(metrics.scale(spacing::TIGHT))
        .p(metrics.scale(spacing::SNUG))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(edge)
        .elevation(Elevation::Raised)
        .text_size(metrics.label_size())
}

/// The heading of an [`inline_form`]: the action's icon on its tone's tint and the
/// plain words of the question.
pub fn form_heading(
    tone: Tone,
    icon: Icon,
    title: impl Into<SharedString>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(metrics.hit_target())
                .rounded(px(radius::MEDIUM))
                .bg(tone.chip_background())
                .child(icons::icon(icon, metrics.small_icon_size(), tone.text())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(metrics.body_size())
                .font_weight(metrics.medium_weight())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(title.into()),
        )
}

/// The label above one field of a form: caption size, medium weight, secondary ink.
pub fn form_label(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .text_size(metrics.caption_size())
        .font_weight(metrics.medium_weight())
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .child(label.into())
}

/// The width of a hairline: a divider, a frame's edge.
pub const HAIRLINE_WIDTH: f32 = 1.0;

/// A hairline from the top of a row of columns to its bottom, between two of them.
pub fn column_divider() -> Div {
    div()
        .flex_none()
        .w(px(HAIRLINE_WIDTH))
        .h_full()
        .bg(theme::swatch(palette::BORDER))
}

/// One segment of a breadcrumb path (ADR-7): quiet words that light under the pointer,
/// the current place in the primary ink and the medium weight. The caller adds the
/// words, its id, its tooltip and its click.
pub fn crumb(current: bool, metrics: Metrics) -> Div {
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let hover_ink = theme::swatch(palette::TEXT_PRIMARY);
    div()
        .flex()
        .flex_none()
        .items_center()
        .min_w_0()
        .px(metrics.scale(spacing::NARROW))
        .py(metrics.scale(spacing::HALF))
        .rounded(px(radius::SMALL))
        .text_size(metrics.label_size())
        .text_color(theme::swatch(if current {
            palette::TEXT_PRIMARY
        } else {
            palette::TEXT_SECONDARY
        }))
        .when(current, |crumb| crumb.font_weight(metrics.medium_weight()))
        .cursor_pointer()
        .hover(move |style| style.bg(hover).text_color(hover_ink))
}

/// The mark between two [`crumb`]s.
pub fn crumb_separator(metrics: Metrics) -> gpui::Svg {
    icons::icon(
        Icon::ChevronRight,
        metrics.caption_size(),
        theme::swatch(palette::TEXT_MUTED),
    )
}

/// What one [`list_row`] shows, left to right.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub struct ListRow {
    /// An avatar or an icon tile ([`leading_icon`]).
    pub leading: Option<AnyElement>,
    /// The first line: the object's name.
    pub title: SharedString,
    /// The second line: an address, a path or a short status. It is words rather than
    /// an element so it ends in an ellipsis where the row is too narrow.
    pub subtitle: Option<SharedString>,
    /// Whether the second line is an endpoint, set in the terminal's monospace face.
    pub monospace_subtitle: bool,
    /// Quiet information at the right end: a tag, a time, a status dot.
    pub meta: Option<AnyElement>,
    /// The row's own commands.
    pub actions: Option<AnyElement>,
    /// Whether the commands only show under the pointer or on the chosen row.
    pub reveal_actions: bool,
}

/// A row of a list of saved things: a leading avatar or icon, two lines of words, quiet
/// metadata on the right and the row's commands after it (ADR-2).
///
/// The words give way before anything else: the title and the second line end in an
/// ellipsis, never in a cut letter, and the metadata and the commands keep their size.
/// With `reveal_actions` the commands are drawn only under the pointer and on the chosen
/// row, so a long list stays quiet.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn list_row(row: ListRow, state: ControlState, metrics: Metrics) -> Div {
    let ListRow {
        leading,
        title,
        subtitle,
        monospace_subtitle,
        meta,
        actions,
        reveal_actions,
    } = row;
    let shown = !reveal_actions || state == ControlState::Selected;
    let text = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(metrics.scale(spacing::HALF))
        // Each line is the one item of a row of its own: a line that ends in an
        // ellipsis is shaped at the width a row gives it, where a column's item was
        // shaped at a narrower pass and drew only the ellipsis.
        .child(
            div().flex().min_w_0().child(
                div()
                    .debug_selector(|| "list.row.title".to_owned())
                    .min_w_0()
                    .truncate()
                    .text_size(metrics.body_size())
                    .font_weight(metrics.medium_weight())
                    .child(line_text(title)),
            ),
        )
        .children(subtitle.map(|subtitle| {
            div().flex().min_w_0().child(
                div()
                    .min_w_0()
                    .truncate()
                    .when(monospace_subtitle, |line| {
                        line.font_family(crate::app::terminal::monospace_family())
                    })
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(line_text(subtitle)),
            )
        }));
    list_row_frame(state, metrics)
        .children(leading)
        .child(text)
        .children(meta.map(|meta| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(meta)
        }))
        .children(actions.map(|actions| {
            let actions = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::HALF))
                .child(actions);
            if shown {
                actions
            } else {
                actions
                    .invisible()
                    .group_hover(ROW_GROUP, |style| style.visible())
            }
        }))
}

/// A square tile holding an icon in a tone's tint, at the head of a [`list_row`].
pub fn leading_icon(icon: Icon, tone: Tone, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.control_height())
        .rounded(px(radius::MEDIUM))
        .bg(tone.chip_background())
        .child(icons::icon(icon, metrics.icon_size(), tone.text()))
}

/// One entry of a sidebar: an icon, a label, an optional count, and the chosen state
/// (ADR-2).
///
/// The chosen entry takes the selected fill and the primary ink; the others are
/// secondary until pointed at. The count is quiet and right-aligned, and the label gives
/// way to it with an ellipsis.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn sidebar_item(
    icon: Icon,
    label: impl Into<SharedString>,
    count: Option<usize>,
    selected: bool,
    metrics: Metrics,
) -> Div {
    let (fill, ink) = if selected {
        (Some(palette::SURFACE_SELECTED), palette::TEXT_PRIMARY)
    } else {
        (None, palette::TEXT_SECONDARY)
    };
    let hover_fill = theme::swatch(palette::SURFACE_HOVER);
    let hover_ink = theme::swatch(palette::TEXT_PRIMARY);
    let ink_color = theme::swatch(ink);
    div()
        .group(ICON_GROUP)
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .w_full()
        .min_w_0()
        .h(metrics.control_height())
        .px(metrics.scale(spacing::TIGHT))
        .rounded(px(radius::MEDIUM))
        .when_some(fill, |item, fill| item.bg(theme::swatch(fill)))
        .text_size(metrics.label_size())
        .text_color(ink_color)
        .when(selected, |item| item.font_weight(metrics.medium_weight()))
        .cursor_pointer()
        .when(!selected, |item| {
            item.hover(move |style| style.bg(hover_fill).text_color(hover_ink))
        })
        .child(
            icons::icon(icon, metrics.icon_size(), ink_color)
                .group_hover(ICON_GROUP, move |style| style.text_color(hover_ink)),
        )
        .child(div().flex_1().min_w_0().truncate().child(line_text(label)))
        .children(count.map(|count| {
            div()
                .flex_none()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(SharedString::from(count.to_string()))
        }))
}

/// A sidebar entry folded to its icon, for the collapsed sidebar: a square target with
/// the chosen fill when it is the current one. Its label is the caller's tooltip.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn sidebar_icon_item(icon: Icon, selected: bool, metrics: Metrics) -> Div {
    let size = metrics.control_height();
    let ink = theme::swatch(if selected {
        palette::TEXT_PRIMARY
    } else {
        palette::TEXT_SECONDARY
    });
    let hover_fill = theme::swatch(palette::SURFACE_HOVER);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(px(radius::MEDIUM))
        .when(selected, |item| {
            item.bg(theme::swatch(palette::SURFACE_SELECTED))
        })
        .when(!selected, |item| {
            item.hover(move |style| style.bg(hover_fill))
        })
        .cursor_pointer()
        .child(icons::icon(icon, metrics.icon_size(), ink))
}

/// The least height of a [`toolbar`]: a control and the bar's own padding.
const TOOLBAR_MIN_HEIGHT: f32 = 44.0;

/// The bar of controls at the head of a page or a column (ADR-2): one line, items
/// centred, a hairline under it. The caller adds the title, the search field and the
/// commands; a child with `flex_1` takes the room between them.
pub fn toolbar(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .w_full()
        .min_w_0()
        .min_h(metrics.scale(TOOLBAR_MIN_HEIGHT))
        .px(metrics.scale(spacing::SNUG))
        .py(metrics.scale(spacing::NARROW))
        .border_b_1()
        .border_color(theme::swatch(palette::BORDER))
}

/// The bar a state is stated in above the content it is about, without covering it
/// (ADR-2, ADR-6): a tone icon, one sentence, and the caller's commands after it.
///
/// It is one line high at rest; the sentence ends in an ellipsis rather than pushing the
/// commands off the bar, and the caller keeps the whole sentence in a tooltip or in a
/// detail it unfolds under the bar. The tone's tint is the fill and a hairline closes it
/// off from what is under it.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn inline_banner(
    tone: Tone,
    icon: Icon,
    message: impl Into<SharedString>,
    metrics: Metrics,
) -> Div {
    inline_banner_frame(tone, icon, metrics).child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .child(line_text(message)),
    )
}

/// The [`inline_banner`] without its sentence: the tint, the hairline and the tone's
/// icon, for a caller that draws its own sentence - one with a tooltip and a selector
/// of its own - and its commands after it.
pub fn inline_banner_frame(tone: Tone, icon: Icon, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .w_full()
        .min_w_0()
        .min_h(metrics.large_control_height())
        .pl(metrics.scale(spacing::SNUG))
        .pr(metrics.scale(spacing::NARROW))
        .py(metrics.scale(spacing::HALF))
        .bg(tone.chip_background())
        .border_b_1()
        .border_color(theme::faded(tone.text(), 0.24))
        .text_size(metrics.label_size())
        .child(icons::icon(icon, metrics.small_icon_size(), tone.text()))
}

/// How far a piece of work has come, for a [`progress_bar`].
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Progress {
    /// A known fraction, from 0 to 1.
    Determinate(f32),
    /// Work whose length is not known: a segment that travels the track.
    Indeterminate,
}

/// How long the travelling segment of an indeterminate [`progress_bar`] takes to cross.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub const PROGRESS_SWEEP: Duration = Duration::from_millis(1200);

/// A thin progress bar: the groove and the part that is done (ADR-2).
///
/// A determinate bar fills to its fraction. An indeterminate one sends a segment along
/// the groove while motion is allowed, and holds it still in the middle when motion is
/// reduced, so the bar still says "working" without moving. `id` keys the sweep.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn progress_bar(id: impl Into<ElementId>, progress: Progress, metrics: Metrics) -> AnyElement {
    let track = div()
        .relative()
        .flex_none()
        .w_full()
        .h(metrics.scale(spacing::HAIRLINE))
        .rounded_full()
        .overflow_hidden()
        .bg(theme::swatch(palette::LOADING_TRACK));
    let fill = div()
        .absolute()
        .top(px(0.0))
        .bottom(px(0.0))
        .rounded_full()
        .bg(theme::swatch(palette::LOADING_FILL));
    match progress {
        Progress::Determinate(fraction) => track
            .debug_selector(|| "progress.determinate".to_owned())
            .child(
                fill.left(px(0.0))
                    .w(gpui::relative(fraction.clamp(0.0, 1.0))),
            )
            .into_any_element(),
        Progress::Indeterminate if theme::motion_allowed() => track
            .debug_selector(|| "progress.indeterminate".to_owned())
            .child(
                fill.w(gpui::relative(0.3)).with_animation(
                    id,
                    Animation::new(PROGRESS_SWEEP)
                        .repeat()
                        .with_easing(gpui::ease_in_out),
                    |fill, delta| fill.left(gpui::relative(delta * 1.3 - 0.3)),
                ),
            )
            .into_any_element(),
        Progress::Indeterminate => track
            .debug_selector(|| "progress.still".to_owned())
            .child(fill.left(gpui::relative(0.35)).w(gpui::relative(0.3)))
            .into_any_element(),
    }
}

/// The start page's progress line (R3): a 2 px determinate bar, fully rounded, filled in
/// the accent, whose fill grows from `from` to `to` over `duration` with the entrance
/// curve, so a bar that steps forward eases into its new value instead of jumping.
///
/// `id` keys the growth: a new `id` plays it again, the same one draws the bar at `to`.
/// With motion reduced, or `still`, the bar is drawn at `to` at once.
pub fn progress_line(
    id: impl Into<ElementId>,
    from: f32,
    to: f32,
    duration: Duration,
    still: bool,
    metrics: Metrics,
) -> AnyElement {
    let to = to.clamp(0.0, 1.0);
    let from = from.clamp(0.0, 1.0);
    let fill = div()
        .absolute()
        .top(px(0.0))
        .bottom(px(0.0))
        .left(px(0.0))
        .rounded_full()
        .bg(theme::swatch(palette::ACCENT));
    let fill = if still || !theme::motion_allowed() || (to - from).abs() < f32::EPSILON {
        fill.w(gpui::relative(to)).into_any_element()
    } else {
        fill.w(gpui::relative(from))
            .with_animation(
                id,
                Animation::new(duration).with_easing(theme::ease_out()),
                move |fill, delta| fill.w(gpui::relative(from + (to - from) * delta)),
            )
            .into_any_element()
    };
    div()
        .relative()
        .flex_none()
        .w_full()
        .h(metrics.scale(spacing::HALF))
        .rounded_full()
        .overflow_hidden()
        .bg(theme::swatch(palette::LOADING_TRACK))
        .debug_selector(|| "progress.line".to_owned())
        .child(fill)
        .into_any_element()
}

/// A placeholder line of text while the words load: a rounded block at `fraction` of
/// the width it is given (ADR-2).
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn skeleton_text(fraction: f32, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .w(gpui::relative(fraction.clamp(0.05, 1.0)))
        .h(metrics.scale(10.0))
        .rounded(px(radius::SMALL))
        .bg(theme::swatch(palette::SURFACE_SELECTED))
}

/// A placeholder [`list_row`] while a list loads: an avatar block and two lines.
///
/// While motion is allowed it breathes once per pulse; reduced motion draws it still.
/// `id` keys the breath.
#[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
pub fn skeleton_row(id: impl Into<ElementId>, metrics: Metrics) -> AnyElement {
    let row = list_row_frame(ControlState::Resting, metrics)
        .debug_selector(|| "skeleton.row".to_owned())
        .child(
            div()
                .flex_none()
                .size(metrics.control_height())
                .rounded(px(radius::MEDIUM))
                .bg(theme::swatch(palette::SURFACE_SELECTED)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(metrics.scale(spacing::NARROW))
                .child(skeleton_text(0.4, metrics))
                .child(skeleton_text(0.7, metrics)),
        );
    if theme::motion_allowed() {
        row.with_animation(
            id,
            Animation::new(theme::Motion::PULSE * 2)
                .repeat()
                .with_easing(gpui::pulsating_between(0.55, 1.0)),
            |row, delta| row.opacity(delta),
        )
        .into_any_element()
    } else {
        row.into_any_element()
    }
}

/// A checkbox box in a pointer-sized target: the shared [`checkbox_box`], lit by the
/// pointer anywhere on its target, for the selection column of a list.
pub fn checkbox_target(checked: bool, metrics: Metrics) -> Div {
    div()
        .group(CHECK_GROUP)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.hit_target())
        .cursor_pointer()
        .child(checkbox_box(checked, metrics))
}

/// A [`switch`] with its state word before it, for a setting that is on or off.
///
/// The word says the state in text as well, so it is not told by the knob's side and
/// colour alone. The caller gives the whole of it an id, a selector and the click; the
/// word carries `<selector>.state`.
pub fn labelled_switch(
    on: bool,
    word: impl Into<SharedString>,
    selector: &'static str,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .min_h(metrics.hit_target())
        .cursor_pointer()
        .child(
            div()
                .min_w(metrics.scale(24.0))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(if on {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_SECONDARY
                }))
                .debug_selector(move || format!("{selector}.state"))
                .child(word.into()),
        )
        .child(switch(on, metrics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::theme::contrast;

    /// Minimum contrast for a label or body line (WCAG AA).
    const BODY: f64 = 4.5;
    /// Minimum contrast for an edge, a mark or an inactive control.
    const GRAPHIC: f64 = 3.0;

    const VARIANTS: [Variant; 5] = Variant::ALL;

    /// The layers a control can be drawn on.
    const LAYERS: [Token; 5] = [
        palette::CANVAS,
        palette::CHROME,
        palette::SURFACE,
        palette::RAISED,
        palette::OVERLAY,
    ];

    /// What a face's label is read against: its own opaque fill; its veil over every
    /// layer; or, with no fill, every layer and a chosen row on every layer.
    fn backdrops(face: ControlFace) -> Vec<Token> {
        match face.fill_token() {
            Some(fill) if fill.is_opaque() => vec![fill],
            Some(veil) => LAYERS.iter().map(|layer| veil.over(*layer)).collect(),
            None => LAYERS
                .iter()
                .copied()
                .chain(
                    LAYERS
                        .iter()
                        .map(|layer| palette::SURFACE_SELECTED.over(*layer)),
                )
                .collect(),
        }
    }

    /// Asserts a face's label reaches its floor in both palettes.
    ///
    /// An inactive control is exempt from WCAG AA, but its word is still read: it keeps
    /// the graphic floor instead of disappearing into its own fill.
    fn assert_readable(name: &str, face: ControlFace) {
        let floor = if face.text_token() == palette::DISABLED_TEXT {
            GRAPHIC
        } else {
            BODY
        };
        for background in backdrops(face) {
            let (dark, washed) = contrast::both(face.text_token(), background);
            assert!(
                dark >= floor,
                "{name}: the label on {background:x} is {dark:.2}:1, below {floor:.1}:1"
            );
            assert!(
                washed >= floor,
                "{name}: the label in the light palette is {washed:.2}:1, below {floor:.1}:1"
            );
        }
    }

    /// Everything a face draws, as one comparable key.
    type Key = (
        Option<Token>,
        Option<Token>,
        Token,
        i32,
        Option<Token>,
        bool,
        bool,
    );

    fn key(face: ControlFace) -> Key {
        (
            face.fill_token(),
            face.border_token(),
            face.text_token(),
            (face.border_width_px() * 10.0) as i32,
            face.mark_token(),
            face.is_busy(),
            face.is_pressed(),
        )
    }

    fn assert_states_differ(name: &str, faces: &[(ControlState, ControlFace)]) {
        for (index, (state, face)) in faces.iter().enumerate() {
            for (other_state, other) in faces.iter().skip(index + 1) {
                assert_ne!(
                    key(*face),
                    key(*other),
                    "{name}: {state:?} and {other_state:?} are drawn the same way"
                );
            }
        }
    }

    fn every_state(face: fn(ControlState) -> ControlFace) -> Vec<(ControlState, ControlFace)> {
        ControlState::ALL
            .into_iter()
            .map(|state| (state, face(state)))
            .collect()
    }

    #[test]
    fn every_button_state_keeps_its_label_readable() {
        for variant in VARIANTS {
            for state in ControlState::ALL {
                assert_readable(
                    &format!("{variant:?} {state:?}"),
                    button_face(variant, state),
                );
            }
        }
    }

    #[test]
    fn a_state_is_never_told_by_colour_alone() {
        for variant in VARIANTS {
            let faces: Vec<(ControlState, ControlFace)> = ControlState::ALL
                .into_iter()
                .map(|state| (state, button_face(variant, state)))
                .collect();
            assert_states_differ(&format!("{variant:?}"), &faces);

            let resting = button_face(variant, ControlState::Resting);
            // Focus doubles the edge and leaves the fill where it was.
            let focused = button_face(variant, ControlState::Focused);
            assert!(
                focused.border_width_px() >= 2.0,
                "{variant:?} Focused only tints its edge"
            );
            assert_eq!(
                focused.fill_token(),
                resting.fill_token(),
                "{variant:?} Focused also changes its fill"
            );
            // Rejection doubles the edge as well, in a different colour, and is allowed
            // to take the accent fill away: an action that cannot run must not look
            // runnable.
            let invalid = button_face(variant, ControlState::Invalid);
            assert!(
                invalid.border_width_px() >= 2.0,
                "{variant:?} Invalid only tints its edge"
            );
            assert_ne!(
                invalid.border_token(),
                focused.border_token(),
                "{variant:?} draws focus and rejection in one colour"
            );
            // A chosen control carries a leading bar; a waiting one carries an
            // indicator; a pressed one thickens its bottom edge.
            assert!(
                button_face(variant, ControlState::Selected)
                    .mark_token()
                    .is_some(),
                "{variant:?} Selected has no mark"
            );
            assert!(button_face(variant, ControlState::Loading).is_busy());
            assert!(button_face(variant, ControlState::Pressed).is_pressed());
            for state in [
                ControlState::Resting,
                ControlState::Hovered,
                ControlState::Disabled,
            ] {
                let face = button_face(variant, state);
                assert!(!face.is_busy() && !face.is_pressed() && face.mark_token().is_none());
            }
        }
    }

    #[test]
    fn the_focus_and_rejection_edges_reach_the_graphic_floor() {
        for variant in VARIANTS {
            for state in [ControlState::Focused, ControlState::Invalid] {
                let face = button_face(variant, state);
                let edge = face.border_token().expect("the state draws an edge");
                for background in backdrops(face) {
                    let (dark, washed) = contrast::both(edge, background);
                    assert!(
                        dark >= GRAPHIC,
                        "{variant:?} {state:?}: the edge on {background:x} is {dark:.2}:1"
                    );
                    assert!(
                        washed >= GRAPHIC,
                        "{variant:?} {state:?}: the edge in the light palette is {washed:.2}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn the_field_row_and_menu_faces_cover_every_state() {
        for (name, faces) in [
            ("field", every_state(field_face)),
            ("row", every_state(row_face)),
            ("menu item", every_state(menu_item_face)),
            ("list row", every_state(list_row_face)),
        ] {
            assert_states_differ(name, &faces);
            for (state, face) in faces {
                assert_readable(&format!("{name} {state:?}"), face);
            }
        }
        assert_readable("dialog", dialog_face());
        for face in [
            field_face(ControlState::Focused),
            row_face(ControlState::Focused),
        ] {
            assert!(
                face.border_width_px() >= 2.0,
                "a focused surface keeps the wide edge"
            );
        }
    }

    #[test]
    fn every_shared_piece_builds_in_every_state() {
        let metrics = Metrics::default();
        for variant in VARIANTS {
            for size in ButtonSize::ALL {
                let _ = button_sized("Connect", variant, size, metrics);
            }
        }
        for state in ControlState::ALL {
            let _ = search_field(
                div().into_any_element(),
                Some(icon_only_button(Icon::Close, metrics).into_any_element()),
                Some("Ctrl+F".into()),
                state,
                metrics,
            );
            let _ = list_row(
                ListRow {
                    leading: Some(
                        leading_icon(Icon::Server, Tone::Accent, metrics).into_any_element(),
                    ),
                    title: "build-01".into(),
                    subtitle: Some("deploy@192.0.2.11".into()),
                    monospace_subtitle: true,
                    meta: Some(tag("Prod", metrics).into_any_element()),
                    actions: Some(
                        compact_button("Connect", Variant::Primary, metrics).into_any_element(),
                    ),
                    reveal_actions: true,
                },
                state,
                metrics,
            );
        }
        for selected in [false, true] {
            let _ = sidebar_item(Icon::Server, "All hosts", Some(24), selected, metrics);
            let _ = sidebar_icon_item(Icon::Server, selected, metrics);
        }
        let _ = toolbar(metrics).child(kbd("Ctrl+K", metrics));
        for tone in [Tone::Danger, Tone::Warning, Tone::Info, Tone::Positive] {
            let _ = inline_banner(tone, Icon::Plug, "Connection lost", metrics);
        }
        for motion in [theme::Motion::Full, theme::Motion::Reduced] {
            theme::set_motion(motion);
            let _ = progress_bar("progress", Progress::Determinate(0.4), metrics);
            let _ = progress_bar("progress", Progress::Indeterminate, metrics);
            let _ = skeleton_row("skeleton", metrics);
        }
        theme::set_motion(theme::Motion::Full);
        let _ = skeleton_text(0.5, metrics);
        let _ = checkbox_target(true, metrics);
        let _ = labelled_switch(true, "On", "switch", metrics);
        for variant in VARIANTS {
            for state in ControlState::ALL {
                let _ = button_with_state("Connect", variant, state, metrics);
            }
        }
        for state in ControlState::ALL {
            let _ = field_frame(state, metrics);
            let _ = row_with_state(state, metrics);
        }
        let _ = menu_panel(metrics)
            .child(menu_item("Copy", false, metrics))
            .child(menu_item("Paste", true, metrics));
        let _ = dialog_panel(metrics)
            .child(dialog_header("New host", "hosts.json", metrics))
            .child(endpoint("deploy@10.0.0.12:22", metrics));
        let _ = notice(
            Tone::Danger,
            "Connection failed",
            "The host key changed; review it before connecting again.",
            metrics,
        );
        let _ = loading_indicator(metrics);
        let _ = card(metrics).child(stacked("prod-web", "10.0.0.12", metrics));
        for on in [false, true] {
            let _ = switch(on, metrics);
            let _ = checkbox("check", "Remember", on, metrics);
            let _ = checkbox_box(on, metrics);
        }
        let _ = menu_card("menu", metrics)
            .child(menu_entry(
                "Open",
                "menu.open",
                Some(Icon::Terminal),
                true,
                false,
                metrics,
            ))
            .child(menu_divider(metrics))
            .child(menu_entry(
                "Remove",
                "menu.remove",
                None,
                true,
                true,
                metrics,
            ))
            .child(menu_entry(
                "Close",
                "menu.close",
                None,
                false,
                false,
                metrics,
            ));
        let _ = menu_enter(
            menu_card("menu", metrics)
                .child(menu_section("Local", metrics))
                .child(
                    menu_entry("Copy", "menu.copy", None, true, false, metrics)
                        .child(menu_shortcut("Ctrl+Shift+C", metrics)),
                ),
            "menu.enter",
        );
        let _ = floating_bar(metrics).child(compact_button("Fit", Variant::Ghost, metrics));
        let _ = dialog_surface().child(div());
        let _ = empty_state(
            Icon::Server,
            "No hosts yet",
            Some("Add a host to connect to it with one click.".into()),
            Some(button("New host", Variant::Primary, metrics).into_any_element()),
            metrics,
        );
        let _ = empty_state(Icon::Layers, "Nothing recent", None, None, metrics);
        let _ = modal(
            gpui::size(px(720.0), px(480.0)),
            "dialog",
            440.0,
            ModalContent {
                header: dialog_header("New host", "hosts.json", metrics),
                subtitle: None,
                body: div().into_any_element(),
                footer: Some(div().child(button("Save", Variant::Primary, metrics))),
            },
            metrics,
        );
        for tone in [
            Tone::Neutral,
            Tone::Accent,
            Tone::Positive,
            Tone::Warning,
            Tone::Danger,
            Tone::Planned,
        ] {
            let _ = chip(tone_word(tone), tone, metrics);
            let _ = status_mark(tone, metrics.scale(7.0));
            let _ = toast(
                tone,
                Icon::Check,
                div().child(tone_word(tone)),
                None,
                metrics,
            );
        }
    }

    #[test]
    fn a_switch_and_a_checkbox_say_their_state_by_shape_as_well_as_colour() {
        // The knob says the state by where it sits, so it must be seen on every track,
        // hovered or not, in both palettes.
        for track in [
            palette::TRACK_OFF,
            palette::TRACK_OFF_HOVER,
            palette::TRACK_ON,
            palette::TRACK_ON_HOVER,
        ] {
            let (dark, light) = contrast::both(palette::KNOB, track);
            assert!(
                dark >= GRAPHIC && light >= GRAPHIC,
                "the knob fades into {track:x}: {dark:.2}/{light:.2}"
            );
        }
        // The on track also says "on" by colour against whatever the switch sits on;
        // the off track is at least told apart from it.
        for surface in [
            palette::SURFACE,
            palette::CHROME,
            palette::RAISED,
            palette::OVERLAY,
        ] {
            let (dark, light) = contrast::both(palette::TRACK_ON, surface);
            assert!(
                dark >= GRAPHIC && light >= GRAPHIC,
                "the on track fades into {surface:x}: {dark:.2}/{light:.2}"
            );
            let (dark, light) = contrast::both(palette::TRACK_OFF, surface);
            assert!(
                dark >= 1.3 && light >= 1.3,
                "the off track disappears into {surface:x}"
            );
        }
        // The tick on a checked box keeps the body floor on the accent fill.
        let (dark, light) = contrast::both(palette::ON_ACCENT, palette::ACCENT);
        assert!(dark >= BODY && light >= BODY);
    }

    #[test]
    fn the_floating_layers_are_drawn_above_the_page() {
        // A menu, a toast and a dialog stand on their own layer and carry a strong edge,
        // so they read as over the page in both palettes even before their shadow.
        let dialog = dialog_face();
        assert_eq!(dialog.fill_token(), Some(palette::OVERLAY));
        assert_eq!(dialog.border_token(), Some(palette::BORDER_STRONG));
        for (name, layer) in [("raised", palette::RAISED), ("overlay", palette::OVERLAY)] {
            let (dark, light) = contrast::both(palette::BORDER_STRONG, layer);
            assert!(
                dark > 1.2 && light > 1.2,
                "the {name} edge is not seen: {dark:.2}/{light:.2}"
            );
        }
    }

    fn tone_word(tone: Tone) -> &'static str {
        match tone {
            Tone::Neutral => "Idle",
            Tone::Accent => "Current",
            Tone::Positive => "Connected",
            Tone::Warning => "Connecting",
            Tone::Danger => "Failed",
            Tone::Planned => "Closed",
            Tone::Info => "Info",
        }
    }

    #[test]
    fn reduced_motion_still_draws_the_waiting_and_loading_states() {
        theme::set_motion(theme::Motion::Reduced);
        let metrics = Metrics::default();
        let _ = loading_indicator(metrics);
        let _ = state_card(
            Tone::Warning,
            "Loading",
            "Reading hosts.json",
            None,
            metrics,
        );
        let _ = button_with_state(
            "Connecting",
            Variant::Secondary,
            ControlState::Loading,
            metrics,
        );
        assert!(!theme::motion_allowed());
        theme::set_motion(theme::Motion::Full);
        assert!(theme::motion_allowed());
    }

    /// A column of a given width that holds one piece, for the layout tests below.
    struct Column {
        width: f32,
        piece: fn(Metrics) -> AnyElement,
    }

    impl Render for Column {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .items_center()
                .w(px(self.width))
                .debug_selector(|| "column".to_owned())
                .child((self.piece)(Metrics::default()))
        }
    }

    fn lay_out(
        cx: &mut gpui::TestAppContext,
        width: f32,
        piece: fn(Metrics) -> AnyElement,
    ) -> &mut gpui::VisualTestContext {
        theme::set_motion(theme::Motion::Reduced);
        let (_view, cx) = cx.add_window_view(move |_window, _cx| Column { width, piece });
        cx.simulate_resize(gpui::size(px(800.0), px(900.0)));
        cx.run_until_parked();
        cx
    }

    /// A diagnostic as long as a real one: one unbroken path, then a sentence.
    const LONG_DIAGNOSTIC: &str = concat!(
        "ssh: connect to host build-01.internal.example port 22: Connection timed out ",
        "after 3 keepalives; C:\\Users\\someone\\AppData\\Local\\Temp\\",
        "shellune-pty-0123456789abcdef was closed while the session was still reading ",
        "from it, and the reconnect budget of five attempts is spent"
    );

    #[gpui::test]
    fn a_status_card_is_as_tall_as_the_diagnostic_it_paints(cx: &mut gpui::TestAppContext) {
        // The card is centred in a column wider than its cap. Measured at the column's
        // width and painted at the cap, the diagnostic used to run out of the card's
        // bottom edge (R5); placed through the slot it is laid out where it is painted.
        let cx = lay_out(cx, 760.0, |metrics| {
            status_card_slot(
                status_card(
                    Tone::Danger,
                    icons::icon(Icon::Plug, metrics.icon_size(), Tone::Danger.text())
                        .into_any_element(),
                    div().child("Connection lost"),
                    Some(div().child("The server stopped answering.")),
                    metrics,
                )
                .debug_selector(|| "card".to_owned())
                .child(
                    div()
                        .debug_selector(|| "diagnostic".to_owned())
                        .whitespace_normal()
                        .text_size(metrics.caption_size())
                        .child(LONG_DIAGNOSTIC),
                ),
                metrics,
            )
            .into_any_element()
        });
        let card = cx.debug_bounds("card").expect("the card is drawn");
        let diagnostic = cx
            .debug_bounds("diagnostic")
            .expect("the diagnostic is drawn");
        assert!(
            card.size.width <= px(STATUS_CARD_WIDTH) + px(0.5),
            "the card keeps its cap: {card:?}"
        );
        assert!(
            diagnostic.bottom() <= card.bottom() + px(0.5)
                && diagnostic.right() <= card.right() + px(0.5),
            "the diagnostic runs out of its card: {diagnostic:?} in {card:?}"
        );
        // The wrapped diagnostic is several lines tall, not the one line a full-width
        // measurement would give it.
        assert!(diagnostic.size.height > Metrics::default().caption_size() * 2.0);
        theme::set_motion(theme::Motion::Full);
    }

    /// Asserts `words` were drawn cut, ending in "…", and not cut down to nothing.
    fn assert_cut(words: &str) {
        let shown = drawn::shown(words).unwrap_or_else(|| panic!("{words:?} is not drawn"));
        let kept = shown
            .strip_suffix('…')
            .unwrap_or_else(|| panic!("{words:?} is drawn as {shown:?}, with no ellipsis"));
        assert!(
            !kept.is_empty() && words.starts_with(kept),
            "{words:?} is drawn as {shown:?}"
        );
    }

    #[gpui::test]
    fn cut_words_in_rows_banners_and_sidebars_end_in_a_drawn_ellipsis(
        cx: &mut gpui::TestAppContext,
    ) {
        // A cut line ends in "…", never in a cut letter (R1); a line that fits is drawn
        // whole. The words are read back from the shaped line, not from the box around
        // it, which kept its width while the letters ran under the next element.
        drawn::clear();
        let _cx = lay_out(cx, 280.0, |metrics| {
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(list_row(
                    ListRow {
                        leading: Some(
                            leading_icon(Icon::Server, Tone::Accent, metrics).into_any_element(),
                        ),
                        title: "a-host-name-that-is-much-longer-than-the-row".into(),
                        subtitle: Some("deploy@a-very-long-address.internal.example:2222".into()),
                        monospace_subtitle: true,
                        meta: Some(tag("Prod", metrics).into_any_element()),
                        actions: None,
                        reveal_actions: false,
                    },
                    ControlState::Selected,
                    metrics,
                ))
                .child(list_row(
                    ListRow {
                        leading: None,
                        title: "api-02".into(),
                        subtitle: Some("ops@192.0.2.12".into()),
                        monospace_subtitle: true,
                        meta: None,
                        actions: None,
                        reveal_actions: false,
                    },
                    ControlState::Resting,
                    metrics,
                ))
                .child(
                    inline_banner(
                        Tone::Danger,
                        Icon::Plug,
                        "Connection lost · the server stopped answering keepalives",
                        metrics,
                    )
                    .child(compact_button(
                        "Reconnect",
                        Variant::Primary,
                        metrics,
                    )),
                )
                .child(sidebar_item(
                    Icon::Folder,
                    "Production servers in the Frankfurt data centre",
                    Some(24),
                    true,
                    metrics,
                ))
                .into_any_element()
        });
        for words in [
            "a-host-name-that-is-much-longer-than-the-row",
            "deploy@a-very-long-address.internal.example:2222",
            "Connection lost · the server stopped answering keepalives",
            "Production servers in the Frankfurt data centre",
        ] {
            assert_cut(words);
        }
        for words in ["api-02", "ops@192.0.2.12"] {
            assert_eq!(drawn::shown(words).as_deref(), Some(words), "{words} fits");
        }
        theme::set_motion(theme::Motion::Full);
    }

    #[gpui::test]
    fn a_list_row_and_a_banner_keep_long_words_on_one_line(cx: &mut gpui::TestAppContext) {
        let cx = lay_out(cx, 280.0, |metrics| {
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(
                    list_row(
                        ListRow {
                            leading: Some(
                                leading_icon(Icon::Server, Tone::Accent, metrics)
                                    .into_any_element(),
                            ),
                            title: "a-host-name-that-is-much-longer-than-the-row".into(),
                            subtitle: Some(
                                "deploy@a-very-long-address.internal.example:2222".into(),
                            ),
                            monospace_subtitle: true,
                            meta: Some(tag("Prod", metrics).into_any_element()),
                            actions: None,
                            reveal_actions: false,
                        },
                        ControlState::Resting,
                        metrics,
                    )
                    .debug_selector(|| "row".to_owned()),
                )
                .child(
                    inline_banner(
                        Tone::Danger,
                        Icon::Plug,
                        "Connection lost · the server stopped answering keepalives",
                        metrics,
                    )
                    .debug_selector(|| "banner".to_owned())
                    .child(compact_button(
                        "Reconnect",
                        Variant::Primary,
                        metrics,
                    )),
                )
                .into_any_element()
        });
        let column = cx.debug_bounds("column").expect("the column is drawn");
        let title = cx
            .debug_bounds("list.row.title")
            .expect("the title is drawn");
        assert!(
            title.size.width > px(60.0),
            "the title is shaped at a width it is not drawn at: {title:?}"
        );
        // Two lines for the row and one for the banner: a wrapped word would add a
        // line to either.
        let line = |size: Pixels| size * 1.7;
        let metrics = Metrics::default();
        for (selector, most) in [
            (
                "row",
                line(metrics.body_size())
                    + line(metrics.caption_size())
                    + metrics.scale(spacing::NARROW + spacing::HALF) * 2.0,
            ),
            ("banner", metrics.scale(36.0) + px(1.0)),
        ] {
            let piece = cx.debug_bounds(selector).expect("the piece is drawn");
            assert!(
                piece.right() <= column.right() + px(0.5),
                "{selector} runs past its column: {piece:?} in {column:?}"
            );
            assert!(
                piece.size.height <= most,
                "{selector} wrapped instead of ending in an ellipsis: {piece:?}, most {most:?}"
            );
        }
        theme::set_motion(theme::Motion::Full);
    }

    #[gpui::test]
    fn a_progress_bar_fills_to_its_fraction(cx: &mut gpui::TestAppContext) {
        let cx = lay_out(cx, 400.0, |metrics| {
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap(px(8.0))
                .child(progress_bar(
                    "progress",
                    Progress::Determinate(0.25),
                    metrics,
                ))
                .child(progress_bar("sweep", Progress::Indeterminate, metrics))
                .into_any_element()
        });
        let bar = cx
            .debug_bounds("progress.determinate")
            .expect("the bar is drawn");
        assert_eq!(bar.size.width, px(400.0));
        assert_eq!(bar.size.height, Metrics::default().scale(spacing::HAIRLINE));
        // Under reduced motion an unknown length is still drawn, and still.
        assert!(cx.debug_bounds("progress.still").is_some());
        assert!(cx.debug_bounds("progress.indeterminate").is_none());
        theme::set_motion(theme::Motion::Full);
    }
}
