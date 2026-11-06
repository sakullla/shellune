//! Reusable view pieces of the Shellune shell (R12, R13).
//!
//! These helpers take tokens and copy, and return a styled [`gpui::Div`]; they own
//! appearance only. Interaction (click, focus, actions) is added by the caller, so
//! the same chip or row can be used in a list, a header or a confirmation.
//!
//! Every clickable piece here carries its own hover state, so a control looks the
//! same wherever it is used and the pointer always gets feedback before a click.

use gpui::prelude::*;
use gpui::{AnyView, App, Div, SharedString, Window, div, px};

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
        .rounded(metrics.scale(4.0))
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
        .px(metrics.scale(7.0))
        .rounded(metrics.scale(10.0))
        .bg(tone.chip_background())
        .text_size(metrics.caption_size())
        .text_color(tone.text())
        .child(
            status_dot(tone, metrics)
                .w(metrics.scale(6.0))
                .h(metrics.scale(6.0)),
        )
        .child(label.into())
}

/// A keycap that names a shortcut.
pub fn keycap(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .px(metrics.scale(5.0))
        .py(metrics.scale(1.0))
        .rounded(px(radius::SMALL))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .text_size(metrics.caption_size())
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
        .min_w(metrics.scale(20.0))
        .h(metrics.scale(18.0))
        .px(metrics.scale(6.0))
        .rounded(metrics.scale(9.0))
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
        .flex_none()
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
        .px(metrics.scale(spacing::SNUG))
        .rounded(px(radius::MEDIUM))
        .border_1()
        .text_size(metrics.label_size())
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into());
    style_variant(base, variant)
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
    let size = metrics.compact_control_height();
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
                .border_color(theme::swatch(palette::ACCENT))
                .text_color(theme::swatch(palette::ON_ACCENT))
                .hover(move |style| style.bg(hover).border_color(hover))
        }
        Variant::Soft => {
            let hover = theme::swatch(palette::ACCENT);
            base.bg(theme::swatch(palette::ACCENT_TINT))
                .border_color(theme::swatch(palette::ACCENT_TINT))
                .text_color(theme::swatch(palette::ACCENT_ON_TINT))
                .hover(move |style| style.border_color(hover))
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

/// The header's main button.
///
/// An unavailable action is drawn as a quiet outline with muted text instead of an
/// accent fill, so the shell never looks as if it can do something it cannot.
pub fn action_button(label: impl Into<SharedString>, available: bool, metrics: Metrics) -> Div {
    if available {
        button(label, Variant::Primary, metrics)
    } else {
        button(label, Variant::Secondary, metrics).text_color(theme::swatch(palette::TEXT_MUTED))
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
pub fn segmented(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .flex_wrap()
        .items_center()
        .gap(metrics.scale(2.0))
        .p(metrics.scale(2.0))
        .rounded(px(radius::MEDIUM))
        .bg(theme::swatch(palette::SURFACE))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
}

/// A raised card that groups one form, panel or question.
pub fn card(metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap(metrics.scale(spacing::SNUG))
        .p(metrics.scale(spacing::BASE + 2.0))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
}

/// A card's title, with an optional line under it.
pub fn card_title(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    metrics: Metrics,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(metrics.scale(2.0))
        .min_w(px(0.0))
        .child(
            div()
                .text_size(metrics.title_size())
                .font_weight(metrics.title_weight())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(title.into()),
        )
        .children(subtitle.map(|subtitle| {
            div()
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(subtitle)
        }))
}

/// A short uppercase-style label above a group of controls.
pub fn section_label(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .flex_none()
        .text_size(metrics.caption_size())
        .font_weight(gpui::FontWeight::MEDIUM)
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
        .pl(metrics.scale(spacing::SNUG))
        .pr(metrics.scale(spacing::TIGHT))
        .py(metrics.scale(spacing::TIGHT))
        .rounded(px(radius::MEDIUM))
        .bg(tone.chip_background())
        .border_l(metrics.scale(3.0))
        .border_color(tone.text())
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
                .w(metrics.scale(96.0))
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
    let emblem = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(metrics.scale(36.0))
        .h(metrics.scale(36.0))
        .rounded(px(radius::LARGE))
        .bg(tone.chip_background())
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .child(
            status_dot(tone, metrics)
                .w(metrics.scale(10.0))
                .h(metrics.scale(10.0)),
        );

    let mut text = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .gap(metrics.scale(spacing::HAIRLINE + 2.0))
        .child(
            div()
                .text_size(metrics.title_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .font_weight(metrics.title_weight())
                .child(title.into()),
        )
        .child(
            div()
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .font_weight(metrics.body_weight())
                .child(body.into()),
        );
    if let Some(step) = next_step {
        text = text.child(
            div()
                .mt(metrics.scale(spacing::HAIRLINE))
                .text_size(metrics.label_size())
                .text_color(tone.text())
                .child(step),
        );
    }

    div()
        .debug_selector(|| "state.card".to_owned())
        .flex()
        .items_start()
        .gap(metrics.scale(spacing::BASE))
        .w_full()
        .p(metrics.scale(spacing::LOOSE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .child(emblem)
        .child(text)
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
        .min_h(metrics.list_row_height())
        .px(metrics.scale(spacing::BASE - 2.0))
        .py(metrics.scale(spacing::TIGHT))
        .rounded(px(radius::MEDIUM))
        .bg(theme::swatch(palette::SURFACE_RAISED))
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
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::SMALL))
            .bg(theme::swatch(palette::SURFACE_SELECTED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_md()
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
