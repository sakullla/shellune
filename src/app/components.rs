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
    Window, div, px,
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
    /// A recommended action inside a list or card: an accent tint.
    Soft,
    /// An ordinary command: a raised surface with a hairline border.
    Secondary,
    /// A quiet command that only shows its surface under the pointer.
    Ghost,
    /// An action that removes or replaces something.
    Danger,
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
            Some(palette::ACCENT_HOVER),
            palette::ON_ACCENT,
        ),
        Variant::Soft => ControlFace::new(
            Some(palette::ACCENT_TINT),
            Some(palette::ACCENT_TINT),
            palette::ACCENT_ON_TINT,
        ),
        Variant::Secondary => ControlFace::new(
            Some(palette::SURFACE),
            Some(palette::BORDER_STRONG),
            palette::TEXT_PRIMARY,
        ),
        Variant::Ghost => ControlFace::new(None, None, palette::TEXT_SECONDARY),
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
        Variant::Soft | Variant::Danger => resting.with_border(accent_of(variant)),
        Variant::Secondary => resting
            .with_fill(palette::SURFACE_HOVER)
            .with_border(palette::BORDER_STRONG),
        Variant::Ghost => resting
            .with_fill(palette::SURFACE_HOVER)
            .with_text(palette::TEXT_PRIMARY),
    };
    match state {
        ControlState::Resting => resting,
        ControlState::Hovered => hovered,
        ControlState::Pressed => match variant {
            // The primary button sinks onto a darker accent. The tonal variants keep
            // their hovered tint and thicken the bottom edge instead, which is the
            // press the pointer can see.
            Variant::Primary => ControlFace::new(
                Some(palette::ACCENT_PRESSED),
                Some(palette::ACCENT_PRESSED),
                palette::ON_ACCENT,
            )
            .pressed(),
            Variant::Ghost => resting
                .with_fill(palette::SURFACE_PRESSED)
                .with_text(palette::TEXT_PRIMARY)
                .pressed(),
            Variant::Soft | Variant::Secondary | Variant::Danger => hovered.pressed(),
        },
        ControlState::Selected => match variant {
            Variant::Primary => ControlFace::new(
                Some(palette::ACCENT_PRESSED),
                Some(palette::ACCENT_PRESSED),
                palette::ON_ACCENT,
            )
            .with_mark(palette::ON_ACCENT),
            Variant::Soft => resting
                .with_border(palette::ACCENT)
                .with_mark(palette::ACCENT),
            Variant::Secondary => resting
                .with_fill(palette::SURFACE_SELECTED)
                .with_border(palette::BORDER_STRONG)
                .with_mark(palette::ACCENT),
            Variant::Ghost => resting
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
            Variant::Soft | Variant::Secondary | Variant::Ghost | Variant::Danger => {
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
            Variant::Soft | Variant::Secondary | Variant::Ghost | Variant::Danger => {
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

/// The colour that says "this is the chosen one" for a variant's own tones.
fn accent_of(variant: Variant) -> Token {
    match variant {
        Variant::Danger => palette::DANGER,
        _ => palette::ACCENT,
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

/// The 2px leading bar that says "this one is chosen" without relying on colour.
fn mark_bar(token: Token, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .w(px(2.0))
        .h(metrics.scale(14.0))
        .rounded(px(1.0))
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
            .border(px(1.5))
            .border_color(color),
        StatusMark::Square => div()
            .w(size * 0.86)
            .h(size * 0.86)
            .rounded(px(1.0))
            .bg(color),
        StatusMark::Box => div()
            .w(size * 0.86)
            .h(size * 0.86)
            .rounded(px(1.0))
            .border(px(1.5))
            .border_color(color),
        StatusMark::Bar => div().w(size * 0.34).h(size).rounded(px(1.0)).bg(color),
        StatusMark::Dash => div().w(size).h(px(1.5)).rounded(px(1.0)).bg(color),
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
        .min_w(px(0.0))
        .overflow_hidden()
        .child(div().min_w(px(0.0)).truncate().child(label.into()))
}

/// The pill, tint and mark of a chip, without its words.
fn chip_frame(tone: Tone, metrics: Metrics) -> Div {
    // A soft pill: the tint and the mark carry the tone, so no outline is needed. A
    // neutral chip is a plain word - its dash mark read as a minus sign in front of it.
    div()
        .flex()
        .items_center()
        .gap(metrics.scale(5.0))
        .h(metrics.scale(20.0))
        .px(metrics.scale(8.0))
        .rounded_full()
        .bg(tone.chip_background())
        .border_1()
        .border_color(theme::faded(tone.text(), 0.16))
        .text_size(metrics.caption_size())
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(tone.text())
        .when(tone != Tone::Neutral, |chip| {
            chip.child(status_mark(tone, metrics.scale(6.0)))
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
        .px(metrics.scale(8.0))
        .rounded_full()
        .bg(theme::swatch(palette::SURFACE_SELECTED))
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .child(label.into())
}

/// A keycap that names a shortcut.
pub fn keycap(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(metrics.scale(18.0))
        .px(metrics.scale(5.0))
        .rounded(px(radius::SMALL))
        .bg(theme::swatch(palette::CHROME))
        .border_1()
        .border_b_2()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .text_size(metrics.scale(10.5))
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// A count in a small block, for the rail and section headings.
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
        .px(metrics.scale(6.0))
        .rounded_full()
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
        .px(metrics.scale(5.0))
        .rounded(px(radius::SMALL))
        .bg(theme::swatch(background))
        .text_size(metrics.scale(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::swatch(text))
        .child(label.into())
}

/// A command button in one of the [`Variant`] weights.
///
/// It paints its resting face and answers the pointer with its hovered face; the states
/// the caller owns are reached through [`button_with_state`].
pub fn button(label: impl Into<SharedString>, variant: Variant, metrics: Metrics) -> Div {
    let base = painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .h(metrics.control_height())
            .px(metrics.scale(spacing::BASE - 2.0))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.label_size())
            .font_weight(gpui::FontWeight::MEDIUM)
            .cursor_pointer()
            .child(label.into()),
        button_face(variant, ControlState::Resting),
    );
    lifted(base, variant).hover(hover_style(button_face(variant, ControlState::Hovered)))
}

/// A solid button rests on the page: the primary and the secondary weights carry a low
/// shadow, the tinted and quiet ones lie flat.
fn lifted(base: Div, variant: Variant) -> Div {
    match variant {
        Variant::Primary | Variant::Secondary => base.elevation(Elevation::Low),
        Variant::Soft | Variant::Ghost | Variant::Danger => base,
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
    let base = painted(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .h(metrics.control_height())
            .px(metrics.scale(spacing::BASE - 2.0))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.label_size())
            .font_weight(gpui::FontWeight::MEDIUM)
            .when(state != ControlState::Disabled, |el| el.cursor_pointer())
            .child(label.into()),
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
    let drawing = if variant == Variant::Ghost {
        drawing.group_hover(ICON_GROUP, move |style| style.text_color(hover_text))
    } else {
        drawing
    };
    let base = painted(
        div()
            .group(ICON_GROUP)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .h(metrics.control_height())
            .pl(metrics.scale(spacing::SNUG - 1.0))
            .pr(metrics.scale(spacing::BASE - 2.0))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.label_size())
            .font_weight(gpui::FontWeight::MEDIUM)
            .cursor_pointer()
            .child(drawing)
            .child(label.into()),
        button_face(variant, ControlState::Resting),
    );
    lifted(base, variant).hover(hover_style(button_face(variant, ControlState::Hovered)))
}

/// A square button holding one icon, quiet until pointed at.
pub fn icon_only_button(icon: Icon, metrics: Metrics) -> Div {
    icon_only_button_hover(
        icon,
        metrics,
        theme::swatch(palette::SURFACE_HOVER),
        theme::swatch(palette::TEXT_PRIMARY),
    )
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

fn icon_only_button_hover(icon: Icon, metrics: Metrics, hover: Rgba, hover_text: Rgba) -> Div {
    let size = metrics.hit_target();
    div()
        .group(ICON_GROUP)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(size)
        .h(size)
        .rounded(px(radius::SMALL))
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .cursor_pointer()
        .hover(move |style| style.bg(hover).text_color(hover_text))
        .child(
            icons::icon(
                icon,
                metrics.small_icon_size(),
                theme::swatch(palette::TEXT_SECONDARY),
            )
            .group_hover(ICON_GROUP, move |style| style.text_color(hover_text)),
        )
}

/// The hover group an icon button's drawing follows.
pub const ICON_GROUP: &str = "icon-button";

/// The text colour a variant draws its label in, for icons that sit beside it.
fn variant_text(variant: Variant) -> Rgba {
    theme::swatch(button_face(variant, ControlState::Resting).text_token())
}

/// A compact button for toolbars, sized to sit inside a 36px bar.
pub fn compact_button(label: impl Into<SharedString>, variant: Variant, metrics: Metrics) -> Div {
    button(label, variant, metrics)
        .h(metrics.compact_control_height())
        .px(metrics.scale(spacing::TIGHT + 2.0))
        .text_size(metrics.caption_size())
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
            .rounded(px(radius::SMALL))
            .text_size(metrics.caption_size())
            .cursor_pointer()
            .child(glyph.into()),
        button_face(Variant::Ghost, ControlState::Resting),
    )
    .hover(hover_style(button_face(
        Variant::Ghost,
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
        .px(metrics.scale(spacing::TIGHT + 2.0))
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
            .elevation(Elevation::Low)
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
        .min_w(px(0.0))
        .max_w_full()
        .gap(metrics.scale(2.0))
        .p(metrics.scale(3.0))
        .rounded(px(radius::MEDIUM + 1.0))
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
    let padding = metrics.scale(spacing::TIGHT + 2.0) * 2.0;
    let border = px(2.0);
    let track_pad = metrics.scale(3.0) * 2.0;
    let track_border = px(2.0);
    let cap = settings_row_content_cap(metrics) - track_pad - track_border;
    (text + padding + border).min(cap)
}

/// Border-box width a [`chip`] with `label` in `tone` takes: its words, its padding, its
/// hairline and, for any tone but the neutral one, its mark.
pub(crate) fn chip_reserved_width(label: &str, tone: Tone, metrics: Metrics) -> Pixels {
    let text = reserved_text_width(label, metrics.caption_size());
    let padding = metrics.scale(8.0) * 2.0;
    let border = px(2.0);
    let mark = if tone == Tone::Neutral {
        px(0.0)
    } else {
        metrics.scale(6.0) + metrics.scale(5.0)
    };
    text + padding + border + mark
}

/// Border-box floor for a [`compact_button`] label.
pub(crate) fn compact_label_reserved_width(label: &str, metrics: Metrics) -> Pixels {
    let text = reserved_text_width(label, metrics.caption_size());
    let padding = metrics.scale(spacing::TIGHT + 2.0) * 2.0;
    let border = px(2.0);
    (text + padding + border).min(settings_row_content_cap(metrics))
}

/// Border-box floor for a [`keycap`].
pub(crate) fn keycap_reserved_width(label: &str, metrics: Metrics) -> Pixels {
    let text = reserved_text_width(label, metrics.scale(10.5));
    let padding = metrics.scale(5.0) * 2.0;
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
                        .min_w(px(0.0))
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
        .min_w(px(0.0))
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
            .min_w(px(0.0))
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

    let mut text = div()
        .flex()
        .flex_col()
        .items_center()
        .w_full()
        .min_w(px(0.0))
        .max_w(metrics.scale(480.0))
        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
        .child(
            div()
                .w_full()
                .whitespace_normal()
                .text_size(metrics.title_size() + px(1.0))
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .font_weight(metrics.title_weight())
                .text_center()
                .child(title.into()),
        )
        .child(
            div()
                .w_full()
                .whitespace_normal()
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .font_weight(metrics.body_weight())
                .text_center()
                .child(body.into()),
        );
    if let Some(step) = next_step {
        text = text.child(
            div()
                .mt(metrics.scale(spacing::HAIRLINE))
                .text_size(metrics.label_size())
                .text_color(match tone {
                    Tone::Neutral | Tone::Planned => theme::swatch(palette::TEXT_MUTED),
                    tone => tone.text(),
                })
                .text_center()
                .child(step),
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
        .elevation(Elevation::Low)
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
        .elevation(Elevation::Mid)
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
        .child(div().flex_1().min_w(px(0.0)).truncate().child(label.into()))
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
        .gap(metrics.scale(2.0))
        .p(metrics.scale(spacing::HAIRLINE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .elevation(Elevation::Mid)
}

/// The hairline between groups of [`menu_entry`]s.
pub fn menu_divider(metrics: Metrics) -> Div {
    div()
        .flex_none()
        .h(px(1.0))
        .mx(metrics.scale(spacing::HAIRLINE))
        .my(metrics.scale(spacing::HAIRLINE - 1.0))
        .bg(theme::swatch(palette::BORDER))
}

/// The panel a choice menu is drawn in: a [`menu_card`] without a selector.
#[cfg_attr(not(test), allow(dead_code))]
pub fn menu_panel(metrics: Metrics) -> Div {
    menu_card("menu.panel", metrics).flex_none().min_w(px(0.0))
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
            .elevation(Elevation::High)
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
            .elevation(Elevation::High),
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
                .min_w(px(0.0))
                .gap(metrics.scale(2.0))
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
            .min_w(px(0.0))
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
        .min_w(px(0.0))
        .gap(metrics.scale(2.0))
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
        .w(px(1.0))
        .h(metrics.scale(16.0))
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
            .elevation(Elevation::Mid)
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
        .min_w(px(0.0))
        .gap(metrics.scale(spacing::SNUG))
        .p(metrics.scale(spacing::BASE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .elevation(Elevation::Low)
}

/// The card a session's state is laid over its terminal in: why it ended, or that it is
/// on its way (R3).
///
/// It floats over the grid, so it is the raised layer at the mid elevation, centred by
/// its caller and no wider than a sentence needs. The emblem tile carries the tone, the
/// title states the state and the optional line says what it means; the caller appends
/// the actions and any disclosure. A press on the card stays on the card: it occludes
/// the grid under it, while the grid around it keeps its pointer.
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
        .w_full()
        .min_w(px(0.0))
        .max_w(metrics.scale(440.0))
        .gap(metrics.scale(spacing::SNUG))
        .p(metrics.scale(spacing::BASE + 4.0))
        .rounded(px(radius::XLARGE))
        .bg(theme::swatch(palette::RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .elevation(Elevation::Mid)
        .cursor_default()
        .child(
            div()
                .flex()
                .items_start()
                .gap(metrics.scale(spacing::SNUG))
                .min_w(px(0.0))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.scale(36.0))
                        .rounded(px(radius::LARGE))
                        .bg(tone.chip_background())
                        .child(emblem),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(metrics.scale(2.0))
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
                .elevation(Elevation::Low),
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
        .elevation(Elevation::Mid)
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
                .min_w(px(0.0))
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
        .min_w(px(0.0))
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
        .child(
            div()
                .w_full()
                .max_w(metrics.scale(420.0))
                .whitespace_normal()
                .text_center()
                .text_size(metrics.title_size())
                .font_weight(metrics.title_weight())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(title.into()),
        )
        .children(body.map(|body| {
            div()
                .w_full()
                .max_w(metrics.scale(420.0))
                .whitespace_normal()
                .text_center()
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .child(body)
        }))
        .children(action.map(|action| div().mt(metrics.scale(spacing::HAIRLINE)).child(action)))
}

/// How much later each object of a group fades in than the one before it.
pub const TURN_DELAY: Duration = Duration::from_millis(40);

/// The last turn that waits longer than the one before it: a long group does not keep
/// its tail waiting, everything after this turn arrives with it.
pub const LAST_TURN: usize = 6;

/// When the `turn`-th object of a group starts to fade in, and when it is fully there.
#[must_use]
pub fn turn_timing(turn: usize) -> (Duration, Duration) {
    let delay = TURN_DELAY * u32::try_from(turn.min(LAST_TURN)).unwrap_or(0);
    (delay, delay + Enter::Rise.duration())
}

/// The curve of the `turn`-th object of a group, over the whole of its [`turn_timing`]:
/// zero while it waits for its turn, then the shared entrance curve over the fade.
pub fn turn_curve(turn: usize) -> impl Fn(f32) -> f32 {
    let (delay, total) = turn_timing(turn);
    let start = delay.as_secs_f32() / total.as_secs_f32();
    let ease = theme::ease_out();
    move |progress| {
        if progress <= start {
            0.0
        } else {
            ease(((progress - start) / (1.0 - start)).min(1.0))
        }
    }
}

/// Fades a group of objects in one after another - the home page's cards - when motion
/// is allowed (R4).
///
/// Each object waits [`TURN_DELAY`] longer than the one before it, then fades in over
/// the card entrance time. Only the opacity moves: the object takes its place, its
/// size and its pointer target from the first frame, so a press during the entrance
/// lands where the eye expects it. Under reduced motion it is drawn as it ends.
pub fn enter_in_turn<E>(element: E, id: impl Into<ElementId>, turn: usize) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    if !theme::motion_allowed() {
        return element.into_any_element();
    }
    let (_, total) = turn_timing(turn);
    element
        .with_animation(
            id,
            Animation::new(total).with_easing(turn_curve(turn)),
            |element, delta| element.opacity(delta),
        )
        .into_any_element()
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
                .min_h(px(0.0))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::theme::contrast;

    /// Minimum contrast for a label or body line (WCAG AA).
    const BODY: f64 = 4.5;
    /// Minimum contrast for an edge, a mark or an inactive control.
    const GRAPHIC: f64 = 3.0;

    const VARIANTS: [Variant; 5] = [
        Variant::Primary,
        Variant::Soft,
        Variant::Secondary,
        Variant::Ghost,
        Variant::Danger,
    ];

    /// Where a face with no fill of its own ends up being drawn.
    const BACKDROPS: [Token; 6] = [
        palette::CANVAS,
        palette::CHROME,
        palette::SURFACE,
        palette::RAISED,
        palette::OVERLAY,
        palette::SURFACE_SELECTED,
    ];

    fn backdrops(face: ControlFace) -> Vec<Token> {
        match face.fill_token() {
            Some(fill) => vec![fill],
            None => BACKDROPS.to_vec(),
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

    #[test]
    fn a_group_fades_in_one_after_another_and_every_turn_ends_fully_there() {
        // The first object starts at once; each later one waits its turn, and a long
        // group stops waiting at the last turn.
        assert_eq!(turn_timing(0).0, Duration::ZERO);
        for turn in 1..=LAST_TURN {
            assert!(turn_timing(turn).0 > turn_timing(turn - 1).0, "turn {turn}");
            assert_eq!(
                turn_timing(turn).1 - turn_timing(turn).0,
                Enter::Rise.duration(),
                "every turn fades over the card entrance time"
            );
        }
        assert_eq!(turn_timing(LAST_TURN + 5), turn_timing(LAST_TURN));
        assert!(turn_timing(LAST_TURN).1 <= Duration::from_millis(500));

        for turn in 0..=LAST_TURN + 1 {
            let curve = turn_curve(turn);
            assert!((curve(1.0) - 1.0).abs() < 1e-4, "turn {turn} ends drawn");
            let mut last = 0.0;
            for step in 0..=20 {
                let value = curve(step as f32 / 20.0);
                assert!(value >= last - 1e-6, "turn {turn} never fades back out");
                last = value;
            }
        }
        // While an earlier card is half in, a later one has not started yet.
        let (delay, total) = turn_timing(2);
        let waiting = delay.as_secs_f32() / total.as_secs_f32() * 0.9;
        assert!(turn_curve(2)(waiting).abs() < f32::EPSILON);
        assert!(turn_curve(0)(waiting) > 0.0);
    }
}
