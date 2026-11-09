//! Short messages about something that just happened, in the bottom-right corner.
//!
//! A toast is for a result the user does not have to act on: a change applied, a
//! path copied, an upload started or that could not start. It never takes room
//! from the page and never sits over it for long: a success leaves after a few
//! seconds, a problem stays a little longer and can be closed at once, and the
//! pointer over the stack holds them while they are read. Anything the user must
//! answer stays where it belongs - in the dialog or beside the item - not here.
//!
//! Every toast speaks in the same tones as the status chips and banners, with the
//! same colours and the same icon per tone.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, SharedString, div, px};

use super::ShelluneView;
use crate::app::components;
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Tone, palette, radius, spacing};

/// How long a success stays.
const SHORT: Duration = Duration::from_secs(4);
/// How long a problem stays.
const LONG: Duration = Duration::from_secs(9);
/// How many toasts show at once; an older one makes way for a newer.
const MOST: usize = 3;

/// One message in the corner.
#[derive(Clone, Debug)]
pub(super) struct Toast {
    id: u64,
    tone: Tone,
    title: String,
    detail: Option<String>,
}

/// The icon a tone carries, here and wherever a state is shown with an icon.
pub(super) fn tone_icon(tone: Tone) -> Icon {
    match tone {
        Tone::Positive => Icon::Check,
        Tone::Danger => Icon::Close,
        Tone::Warning => Icon::Refresh,
        Tone::Neutral | Tone::Accent | Tone::Planned => Icon::More,
    }
}

impl ShelluneView {
    /// Shows a message in the corner for a while.
    pub(super) fn toast(
        &mut self,
        tone: Tone,
        title: impl Into<String>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.next_toast += 1;
        let id = self.next_toast;
        self.toasts.push(Toast {
            id,
            tone,
            title: title.into(),
            detail,
        });
        if self.toasts.len() > MOST {
            self.toasts.remove(0);
        }
        let stay = if tone == Tone::Danger { LONG } else { SHORT };
        self.expire_toast(id, stay, cx);
        cx.notify();
    }

    /// Takes a toast away after `after`, or later while the pointer is over the
    /// stack.
    fn expire_toast(&mut self, id: u64, after: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |view, cx| {
                if view.toasts_hovered {
                    view.expire_toast(id, Duration::from_secs(2), cx);
                } else {
                    view.dismiss_toast(id, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|toast| toast.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// The messages, newest at the bottom, in the corner above the page.
    pub(super) fn toast_stack(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        if self.toasts.is_empty() {
            return None;
        }
        let metrics = self.metrics();
        let mut stack = div()
            .id("toasts")
            .debug_selector(|| "toasts".to_owned())
            .absolute()
            .right(metrics.scale(spacing::LOOSE))
            .bottom(metrics.scale(spacing::LOOSE))
            .flex()
            .flex_col()
            .items_end()
            .gap(metrics.scale(spacing::TIGHT))
            .on_hover(cx.listener(|view, hovered: &bool, _window, cx| {
                view.toasts_hovered = *hovered;
                cx.notify();
            }));
        for (index, toast) in self.toasts.iter().enumerate() {
            let id = toast.id;
            let tone = toast.tone;
            stack = stack.child(
                div()
                    .id(("toast", id as usize))
                    .debug_selector(move || format!("toast.{index}"))
                    .occlude()
                    .flex()
                    .items_start()
                    .gap(metrics.scale(spacing::TIGHT + 2.0))
                    .w(metrics.scale(340.0))
                    .p(metrics.scale(spacing::SNUG))
                    .rounded(px(radius::LARGE))
                    .bg(theme::swatch(palette::SURFACE_RAISED))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER_STRONG))
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(metrics.scale(22.0))
                            .rounded_full()
                            .bg(theme::swatch(tone.tint()))
                            .child(icons::icon(
                                tone_icon(tone),
                                metrics.small_icon_size(),
                                tone.text(),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .gap(metrics.scale(2.0))
                            .child(
                                div()
                                    .text_size(metrics.label_size())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .debug_selector(move || format!("toast.{index}.title"))
                                    .child(SharedString::from(toast.title.clone())),
                            )
                            .children(toast.detail.clone().map(|detail| {
                                div()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .line_clamp(2)
                                    .child(SharedString::from(detail))
                            })),
                    )
                    .child(
                        components::icon_only_button(Icon::Close, metrics)
                            .flex_none()
                            .size(metrics.scale(22.0))
                            .id(("toast.close", id as usize))
                            .on_click(cx.listener(move |view, _event, _window, cx| {
                                view.dismiss_toast(id, cx);
                            })),
                    ),
            );
        }
        Some(stack)
    }

    /// The toasts on screen, oldest first, for a test.
    #[cfg(test)]
    pub fn toast_titles(&self) -> Vec<String> {
        self.toasts
            .iter()
            .map(|toast| toast.title.clone())
            .collect()
    }
}
