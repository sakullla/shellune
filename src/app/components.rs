//! Reusable view pieces of the Shellune shell (R12, R13).
//!
//! These helpers take tokens and copy, and return a styled [`gpui::Div`]; they own
//! appearance only. Interaction (click, focus, actions) is added by the caller, so
//! the same chip or row can be used in a list, a header or a confirmation.

use gpui::prelude::*;
use gpui::{Div, SharedString, div, px};

use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};

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
        .items_center()
        .gap(metrics.scale(spacing::HAIRLINE))
        .px(metrics.scale(spacing::TIGHT))
        .py(metrics.scale(3.0))
        .rounded(px(radius::SMALL))
        .bg(tone.chip_background())
        .text_size(metrics.caption_size())
        .text_color(tone.text())
        .child(status_dot(tone, metrics))
        .child(label.into())
}

/// A keycap shown next to a navigation entry.
pub fn keycap(label: impl Into<SharedString>, metrics: Metrics) -> Div {
    div()
        .px(metrics.scale(spacing::HAIRLINE))
        .py(metrics.scale(1.0))
        .rounded(metrics.scale(4.0))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(label.into())
}

/// A command button.
///
/// An unavailable action is drawn as an outline with muted text instead of an
/// accent fill, so the shell never looks as if it can do something it cannot.
pub fn action_button(label: impl Into<SharedString>, available: bool, metrics: Metrics) -> Div {
    let button = div()
        .flex()
        .items_center()
        .justify_center()
        .h(metrics.scale(30.0))
        .px(metrics.scale(spacing::SNUG))
        .rounded(px(radius::MEDIUM))
        .text_size(metrics.label_size())
        .child(label.into());

    if available {
        button
            .bg(theme::swatch(palette::ACCENT_TINT))
            .text_color(theme::swatch(palette::ACCENT_ON_TINT))
            .border_1()
            .border_color(theme::swatch(palette::ACCENT))
    } else {
        button
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
    }
}

/// The card used for every non-populated section state.
///
/// Empty, loading, failed and planned sections share one skeleton — tone, title,
/// body and an optional next step — so the difference between them is what the
/// copy says and which tone carries it, never the shape of the card.
pub fn state_card(
    tone: Tone,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    next_step: Option<SharedString>,
    metrics: Metrics,
) -> Div {
    let card = div()
        .debug_selector(|| "state.card".to_owned())
        .flex()
        .flex_col()
        .gap(metrics.scale(spacing::TIGHT))
        .w_full()
        .p(metrics.scale(spacing::LOOSE))
        .rounded(px(radius::LARGE))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
        .child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .child(status_dot(tone, metrics))
                .child(
                    div()
                        .text_size(metrics.title_size())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .font_weight(metrics.title_weight())
                        .child(title.into()),
                ),
        )
        .child(
            div()
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .font_weight(metrics.body_weight())
                .child(body.into()),
        );

    match next_step {
        Some(step) => card.child(
            div()
                .mt(metrics.scale(spacing::HAIRLINE))
                .text_size(metrics.label_size())
                .text_color(tone.text())
                .child(step),
        ),
        None => card,
    }
}

/// A list row frame shared by hosts, sessions and transfers.
pub fn row(metrics: Metrics) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(metrics.scale(spacing::SNUG))
        .h(metrics.list_row_height())
        .px(metrics.scale(spacing::SNUG))
        .rounded(px(radius::MEDIUM))
        .bg(theme::swatch(palette::SURFACE_RAISED))
        .border_1()
        .border_color(theme::swatch(palette::BORDER))
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
        .gap(metrics.scale(1.0))
        .child(
            div()
                .text_size(metrics.body_size())
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

/// A count with its unit, for panel headers and the status bar.
pub fn count_label(count: usize, unit: &str, metrics: Metrics) -> Div {
    div()
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_MUTED))
        .child(SharedString::from(format!("{count} {unit}")))
}
