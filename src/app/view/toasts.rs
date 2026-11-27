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
use gpui::{Context, Div, SharedString, div};

use super::ShelluneView;
use crate::app::components;
use crate::app::icons::Icon;
use crate::app::theme::{self, Tone, palette, spacing};

/// How long a success stays.
const SHORT: Duration = Duration::from_secs(4);
/// How long a warning stays.
const MEDIUM: Duration = Duration::from_secs(6);
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
        Tone::Danger => Icon::AlertCircle,
        Tone::Warning => Icon::AlertTriangle,
        Tone::Neutral | Tone::Accent | Tone::Planned | Tone::Info => Icon::Info,
    }
}

impl ShelluneView {
    /// A value a person may want to paste elsewhere - an address, a path, a reason,
    /// a fingerprint - drawn in `base` and copied with one click.
    ///
    /// Text drawn by the shell cannot be dragged over and selected, so the values
    /// worth copying say so under the pointer and go to the clipboard whole, with a
    /// toast that shows what was copied.
    pub(super) fn copyable_text(
        &self,
        selector: &'static str,
        text: impl Into<String>,
        base: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let text: String = text.into();
        let copied = text.clone();
        let hover = theme::swatch(palette::TEXT_PRIMARY);
        base.id(selector)
            .debug_selector(move || selector.to_owned())
            .cursor_pointer()
            .hover(move |style| style.text_color(hover).underline())
            .tooltip(crate::app::components::tooltip(
                crate::app::copy::ux().copy_click_hint,
            ))
            .on_click(cx.listener(move |view, _event, _window, cx| {
                cx.stop_propagation();
                view.copy_text(copied.clone(), cx);
            }))
            .child(gpui::SharedString::from(text))
    }

    /// Puts text on the clipboard and says so.
    pub(crate) fn copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        self.toast(
            Tone::Positive,
            crate::app::copy::ux().toast_copied,
            Some(text),
            cx,
        );
    }

    /// Shows a message in the corner for a while, and says which one it was.
    ///
    /// The id lets a caller take the message away when what it describes stops being
    /// true, instead of leaving a note about a state that is over.
    pub(super) fn toast(
        &mut self,
        tone: Tone,
        title: impl Into<String>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) -> u64 {
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
        let stay = match tone {
            Tone::Danger => LONG,
            Tone::Warning => MEDIUM,
            _ => SHORT,
        };
        self.expire_toast(id, stay, cx);
        cx.notify();
        id
    }

    /// Whether the pointer is over the toast stack or any toast card.
    fn is_toasts_hovered(&self) -> bool {
        self.toasts_hovered || !self.toast_cards_hovered.is_empty()
    }

    /// Takes a toast away after `after`, or later while the pointer is over the
    /// stack.
    fn expire_toast(&mut self, id: u64, after: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |view, cx| {
                if view.is_toasts_hovered() {
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
        self.toast_cards_hovered.remove(&id);
        if self.toasts.is_empty() {
            self.toasts_hovered = false;
            self.toast_cards_hovered.clear();
        }
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// Dismisses the most recent toast, for Escape; returns whether one was dismissed.
    pub(super) fn dismiss_latest_toast(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(last) = self.toasts.pop() else {
            return false;
        };
        self.toast_cards_hovered.remove(&last.id);
        if self.toasts.is_empty() {
            self.toasts_hovered = false;
            self.toast_cards_hovered.clear();
        }
        cx.notify();
        true
    }

    /// The messages, newest at the bottom, in the corner above the page.
    pub(super) fn toast_stack(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        if self.toasts.is_empty() {
            return None;
        }
        let metrics = self.metrics();
        // Lift the stack above the bottom session bar so a toast never covers
        // `terminal.search` or `session.more` in the bottom-right corner.
        let bottom_clearance = metrics
            .hit_target()
            .max(metrics.scale(crate::app::terminal::SHELL_BAR_HEIGHT))
            + metrics.scale(spacing::SNUG);
        let mut stack = div()
            .id("toasts")
            .debug_selector(|| "toasts".to_owned())
            .absolute()
            .right(metrics.scale(spacing::LOOSE))
            .bottom(bottom_clearance)
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
            let title = div()
                .debug_selector(move || format!("toast.{index}.title"))
                .child(SharedString::from(toast.title.clone()));
            let full_detail = toast
                .detail
                .as_deref()
                .filter(|detail| detail.chars().count() > 48 || detail.contains('\n'))
                .map(|detail| SharedString::from(detail.to_owned()));
            let card = components::toast(
                tone,
                tone_icon(tone),
                title,
                toast.detail.clone().map(SharedString::from),
                metrics,
            )
            .id(("toast", id as usize))
            .debug_selector(move || format!("toast.{index}"))
            .occlude()
            .when_some(full_detail, |card, detail| {
                card.tooltip(components::tooltip(detail))
            })
            .on_hover(cx.listener(move |view, hovered: &bool, _window, cx| {
                if *hovered {
                    view.toast_cards_hovered.insert(id);
                } else {
                    view.toast_cards_hovered.remove(&id);
                }
                cx.notify();
            }))
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    .flex_none()
                    .size(metrics.hit_target())
                    .id(("toast.close", id as usize))
                    .debug_selector(move || format!("toast.close.{index}"))
                    .tooltip(components::tooltip(crate::app::copy::ux().toast_dismiss))
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.dismiss_toast(id, cx);
                    })),
            );
            stack = stack.child(card);
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

    /// The tone and title of every toast on screen, oldest first, for a test.
    #[cfg(test)]
    pub fn toast_entries(&self) -> Vec<(Tone, String)> {
        self.toasts
            .iter()
            .map(|toast| (toast.tone, toast.title.clone()))
            .collect()
    }

    /// Whether the pointer is recorded as hovering over the toast stack, for a test.
    #[cfg(test)]
    pub fn toasts_hovered(&self) -> bool {
        self.is_toasts_hovered()
    }
}

#[cfg(test)]
mod tests {
    use super::tone_icon;
    use crate::app::icons::Icon;
    use crate::app::theme::Tone;

    #[test]
    fn tone_icon_uses_dedicated_semantic_icons() {
        assert_eq!(tone_icon(Tone::Positive), Icon::Check);
        assert_eq!(tone_icon(Tone::Warning), Icon::AlertTriangle);
        assert_eq!(tone_icon(Tone::Danger), Icon::AlertCircle);
        assert_eq!(tone_icon(Tone::Info), Icon::Info);
        assert_eq!(tone_icon(Tone::Neutral), Icon::Info);
        assert_eq!(tone_icon(Tone::Accent), Icon::Info);
        assert_eq!(tone_icon(Tone::Planned), Icon::Info);
    }
}
