//! Reusable view pieces of the Shellune shell (R12, R13).
//!
//! These helpers take tokens and copy, and return a styled [`gpui::Div`]; they own
//! appearance only. Interaction (click, focus, actions) is added by the caller, so
//! the same chip or row can be used in a list, a header or a confirmation.
//!
//! Every clickable piece here carries its own hover state, so a control looks the
//! same wherever it is used and the pointer always gets feedback before a click.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Div, Pixels, SharedString, Window, div, px,
};

use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};

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
    /// An ordinary command: a raised surface with a border.
    Secondary,
    /// A quiet command that only shows its surface under the pointer.
    Ghost,
    /// An action that removes or replaces something.
    Danger,
}

/// A small round status indicator.
pub fn status_dot(tone: Tone, metrics: Metrics) -> Div {
    div()
        .w(metrics.scale(7.0))
        .h(metrics.scale(7.0))
        .rounded_full()
        .bg(tone.text())
        .flex_none()
}

/// A pill that carries a state name and its tone.
pub fn chip(label: impl Into<SharedString>, tone: Tone, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(5.0))
        .h(metrics.scale(20.0))
        .px(metrics.scale(8.0))
        .rounded_full()
        .bg(tone.chip_background())
        .text_size(metrics.caption_size())
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(tone.text())
        .child(
            status_dot(tone, metrics)
                .w(metrics.scale(6.0))
                .h(metrics.scale(6.0)),
        )
        .child(label.into())
}

/// A quiet label for a grouping such as a host's group: no dot, because it is not a
/// state.
pub fn tag(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(metrics.scale(20.0))
        .px(metrics.scale(8.0))
        .rounded_full()
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
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
        .rounded(px(radius::SMALL - 1.0))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_b_2()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .text_size(metrics.scale(10.5))
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// A count in a small pill, for the rail and section headings.
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
        .px(metrics.scale(5.0))
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
        .rounded(metrics.scale(4.0))
        .bg(theme::swatch(background))
        .text_size(metrics.scale(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::swatch(text))
        .child(label.into())
}

/// A command button in one of the [`Variant`] weights.
pub fn button(label: impl Into<SharedString>, variant: Variant, metrics: Metrics) -> Div {
    let base = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
        .h(metrics.control_height())
        .px(metrics.scale(spacing::BASE - 2.0))
        .rounded(px(radius::MEDIUM))
        .border_1()
        .text_size(metrics.label_size())
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into());
    style_variant(base, variant)
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
    let hover_text = theme::swatch(palette::TEXT_PRIMARY);
    let drawing = icons::icon(icon, metrics.small_icon_size(), color);
    let drawing = if variant == Variant::Ghost {
        drawing.group_hover(ICON_GROUP, move |style| style.text_color(hover_text))
    } else {
        drawing
    };
    let base = div()
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
        .border_1()
        .text_size(metrics.label_size())
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .child(drawing)
        .child(label.into());
    style_variant(base, variant)
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

/// [`icon_only_button`] for a destructive action: it warns in the danger colour
/// under the pointer, before the click.
pub fn danger_icon_button(icon: Icon, metrics: Metrics) -> Div {
    icon_only_button_hover(
        icon,
        metrics,
        theme::swatch(Tone::Danger.tint()),
        theme::swatch(palette::DANGER),
    )
}

fn icon_only_button_hover(
    icon: Icon,
    metrics: Metrics,
    hover: gpui::Rgba,
    hover_text: gpui::Rgba,
) -> Div {
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
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .cursor_pointer()
        .hover(move |style| style.bg(hover).text_color(hover_text))
        .child(
            icons::icon(
                icon,
                metrics.small_icon_size(),
                theme::swatch(palette::TEXT_MUTED),
            )
            .group_hover(ICON_GROUP, move |style| style.text_color(hover_text)),
        )
}

/// The hover group an icon button's drawing follows.
pub const ICON_GROUP: &str = "icon-button";

/// The text colour a variant draws its label in, for icons that sit beside it.
fn variant_text(variant: Variant) -> gpui::Rgba {
    match variant {
        Variant::Primary => theme::swatch(palette::ON_ACCENT),
        Variant::Soft => theme::swatch(palette::ACCENT_ON_TINT),
        Variant::Secondary => theme::swatch(palette::TEXT_PRIMARY),
        Variant::Ghost => theme::swatch(palette::TEXT_SECONDARY),
        Variant::Danger => Tone::Danger.text(),
    }
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
    style_variant(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .min_w(size)
            .h(size)
            .px(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::SMALL))
            .border_1()
            .text_size(metrics.caption_size())
            .cursor_pointer()
            .child(glyph.into()),
        Variant::Ghost,
    )
}

fn style_variant(base: Div, variant: Variant) -> Div {
    match variant {
        Variant::Primary => {
            let hover = theme::swatch(palette::ACCENT_HOVER);
            base.bg(theme::swatch(palette::ACCENT))
                .border_color(theme::swatch(palette::ACCENT_HOVER))
                .text_color(theme::swatch(palette::ON_ACCENT))
                .shadow_sm()
                .hover(move |style| style.bg(hover))
        }
        Variant::Soft => {
            let hover = theme::swatch(palette::ACCENT);
            base.bg(theme::swatch(palette::ACCENT_TINT))
                .border_color(theme::swatch(palette::ACCENT_TINT))
                .text_color(theme::swatch(palette::ACCENT_ON_TINT))
                .hover(move |style| style.border_color(theme::faded(hover, 0.6)))
        }
        Variant::Secondary => {
            let hover = theme::swatch(palette::SURFACE_HOVER);
            let hover_border = theme::swatch(palette::BORDER_STRONG);
            base.bg(theme::swatch(palette::SURFACE_RAISED))
                .border_color(theme::swatch(palette::BORDER))
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .hover(move |style| style.bg(hover).border_color(hover_border))
        }
        Variant::Ghost => {
            let hover = theme::swatch(palette::SURFACE_HOVER);
            let text = theme::swatch(palette::TEXT_PRIMARY);
            base.border_color(theme::clear())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .hover(move |style| style.bg(hover).text_color(text))
        }
        Variant::Danger => {
            let hover = Tone::Danger.text();
            base.bg(theme::swatch(palette::DANGER_TINT))
                .border_color(theme::swatch(palette::DANGER_TINT))
                .text_color(Tone::Danger.text())
                .hover(move |style| style.border_color(hover))
        }
    }
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
        base.bg(theme::swatch(palette::SURFACE_SELECTED))
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .font_weight(gpui::FontWeight::MEDIUM)
            .shadow_sm()
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
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
        .bg(theme::swatch(palette::CANVAS))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
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

/// A short uppercase-style label above a group of controls.
pub fn section_label(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .text_size(metrics.caption_size())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// A tinted strip that carries a notice, a question or a failure in one tone.
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
    let (emblem_bg, emblem_fg) = match tone {
        Tone::Neutral | Tone::Planned => (
            theme::swatch(palette::SURFACE_SELECTED),
            theme::swatch(palette::TEXT_SECONDARY),
        ),
        tone => (tone.chip_background(), tone.text()),
    };
    let emblem = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(metrics.scale(48.0))
        .h(metrics.scale(48.0))
        .rounded(px(radius::XLARGE - 2.0))
        .bg(emblem_bg)
        .border_1()
        .border_color(theme::swatch(palette::BORDER_STRONG))
        .child(match emblem_icon {
            Some(icon) => icons::icon(icon, metrics.scale(22.0), emblem_fg).into_any_element(),
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
        .rounded(px(radius::XLARGE))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .child(emblem)
        .child(text)
}

/// The dot on a waiting card.
///
/// Full motion fades it in once. Reduced motion draws it at full strength immediately,
/// and the card's words stay either way. The fade is not a loop: a load that takes
/// time is still told by the warning copy, not by something that keeps moving.
fn attention_dot(tone: Tone, metrics: Metrics) -> AnyElement {
    let dot = status_dot(tone, metrics)
        .w(metrics.scale(10.0))
        .h(metrics.scale(10.0));
    if tone == Tone::Warning && theme::motion_allowed() {
        dot.debug_selector(|| "state.motion.pulse".to_owned())
            .with_animation(
                "state.motion",
                Animation::new(Duration::from_millis(180)),
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

/// A list row frame shared by hosts, sessions and transfers.
pub fn row(metrics: Metrics) -> Div {
    let hover = theme::swatch(palette::SURFACE_HOVER);
    let hover_border = theme::swatch(palette::BORDER_STRONG);
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
        .rounded(px(radius::LARGE - 2.0))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .hover(move |style| style.bg(hover).border_color(hover_border))
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
        .bg(theme::swatch(palette::BORDER_STRONG))
}

/// The small floating label a control shows under the pointer.
pub struct Tooltip {
    text: SharedString,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        div()
            .px(metrics.scale(spacing::TIGHT + 1.0))
            .py(metrics.scale(spacing::HAIRLINE + 1.0))
            .rounded(px(radius::SMALL + 1.0))
            .bg(theme::swatch(palette::SURFACE_SELECTED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
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
