//! A thin, draggable scrollbar for any scrolling container that tracks a
//! [`ScrollHandle`].
//!
//! GPUI scrolls a container by wheel and touchpad but draws nothing that says how long
//! the content is or where the view sits in it, and a mouse without a wheel had no
//! way to move at all. This element reads the handle the container already tracks -
//! its bounds, offset and maximum offset are written while the container is laid out -
//! and paints a proportional thumb along its right edge:
//!
//! - It is drawn only while there is something to scroll.
//! - Pointing at the strip widens the thumb; pressing on the thumb drags it, pressing
//!   beside it moves the view there and keeps dragging from that point.
//! - It reuses the ink the terminal's scrollback thumb uses, so every scrollbar in the
//!   window reads as the same control.
//!
//! The bar measures its track from the handle's own bounds, not from where it is laid
//! out: it takes no room and needs no wrapper, so adding it never moves the layout it
//! is added to. It must come after the scrolling container in the tree - elements are
//! prepainted in order, so the handle holds this frame's numbers by the time the bar
//! measures itself.

use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, ElementId, Hitbox, HitboxBehavior, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, Styled,
    UniformListScrollHandle, Window, canvas, point, px, quad, size,
};

use crate::app::theme::{self, palette};

/// The hit strip along the container's right edge at 100% scale.
pub const TRACK_WIDTH: f32 = 12.0;
/// The thumb at rest at 100% scale.
const THUMB_WIDTH: f32 = 4.0;
/// The thumb while pointed at or dragged at 100% scale.
const THUMB_ACTIVE_WIDTH: f32 = 8.0;
/// The gap between the thumb and the container's right edge and its ends at 100% scale.
const THUMB_INSET: f32 = 2.0;
/// The shortest thumb at 100% scale, so a very long list still has something to grab.
const THUMB_MIN_LENGTH: f32 = 28.0;
/// A remainder smaller than this is rounding, not content.
const SCROLL_EPSILON: f32 = 0.5;

/// The scaled hit-strip width for `metrics`.
#[must_use]
pub fn track_width(metrics: theme::Metrics) -> Pixels {
    metrics.scale(TRACK_WIDTH)
}

/// What the bar remembers between frames.
#[derive(Default)]
struct BarState {
    /// Where on the thumb the pointer grabbed it, while a drag is under way.
    grab: Option<Pixels>,
    /// Whether the pointer is over the strip.
    hovered: bool,
}

/// The measured parts of one frame's bar.
#[derive(Clone, Copy)]
struct Geometry {
    track: Bounds<Pixels>,
    inset: Pixels,
    thumb_top: Pixels,
    thumb_length: Pixels,
    travel: Pixels,
    max: Pixels,
}

impl Geometry {
    fn measure(
        handle: &ScrollHandle,
        track: Bounds<Pixels>,
        metrics: theme::Metrics,
    ) -> Option<Self> {
        let max = handle.max_offset().height;
        let viewport = handle.bounds().size.height;
        if max <= px(SCROLL_EPSILON) || viewport <= Pixels::ZERO {
            return None;
        }
        let inset = metrics.scale(THUMB_INSET);
        let min_length = metrics.scale(THUMB_MIN_LENGTH);
        let usable = track.size.height - inset * 2.0;
        if usable <= min_length {
            return None;
        }
        let ratio = viewport / (viewport + max);
        let thumb_length = (usable * ratio).max(min_length).min(usable);
        let travel = usable - thumb_length;
        let progress = (-handle.offset().y / max).clamp(0.0, 1.0);
        Some(Self {
            track,
            inset,
            thumb_top: track.origin.y + inset + travel * progress,
            thumb_length,
            travel,
            max,
        })
    }

    fn thumb_contains(&self, y: Pixels) -> bool {
        y >= self.thumb_top && y <= self.thumb_top + self.thumb_length
    }

    /// The offset that puts the thumb's top at `top`.
    fn offset_for(&self, top: Pixels) -> Pixels {
        if self.travel <= Pixels::ZERO {
            return Pixels::ZERO;
        }
        let progress = ((top - self.track.origin.y - self.inset) / self.travel).clamp(0.0, 1.0);
        -(self.max * progress)
    }
}

/// A scrollbar for a uniform list.
pub fn for_list(id: impl Into<ElementId>, handle: &UniformListScrollHandle) -> impl IntoElement {
    let base = handle.0.borrow().base_handle.clone();
    bar(id, base)
}

/// A scrollbar for a container scrolled with `overflow_y_scroll` and `track_scroll`.
pub fn bar(id: impl Into<ElementId>, handle: ScrollHandle) -> impl IntoElement {
    let id = id.into();
    let paint_handle = handle.clone();
    canvas(
        move |_bounds, window: &mut Window, cx: &mut App| {
            let state = window.use_keyed_state(id, cx, |_, _| BarState::default());
            let metrics = theme::metrics();
            let track_w = track_width(metrics);
            let viewport = handle.bounds();
            let track = Bounds::new(
                point(viewport.right() - track_w, viewport.top()),
                size(track_w, viewport.size.height),
            );
            let geometry = Geometry::measure(&handle, track, metrics);
            let hitbox = geometry
                .map(|_| window.insert_hitbox(track, HitboxBehavior::BlockMouseExceptScroll));
            (state, geometry, hitbox, metrics)
        },
        move |_bounds,
              (state, geometry, hitbox, metrics): (_, Option<Geometry>, Option<Hitbox>, _),
              window,
              cx| {
            let (Some(geometry), Some(hitbox)) = (geometry, hitbox) else {
                return;
            };
            let handle = paint_handle;
            let (dragging, hovered) = {
                let state = state.read(cx);
                (state.grab.is_some(), state.hovered)
            };
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);

            let active = dragging || hovered;
            let width = metrics.scale(if active {
                THUMB_ACTIVE_WIDTH
            } else {
                THUMB_WIDTH
            });
            let ink = if active {
                theme::faded(theme::swatch(palette::TEXT_SECONDARY), 0.78)
            } else {
                theme::faded(theme::swatch(palette::TEXT_MUTED), 0.45)
            };
            let thumb = Bounds::new(
                point(
                    geometry.track.right() - geometry.inset - width,
                    geometry.thumb_top,
                ),
                size(width, geometry.thumb_length),
            );
            window.paint_quad(quad(
                thumb,
                width / 2.0,
                ink,
                Pixels::ZERO,
                gpui::transparent_black(),
                gpui::BorderStyle::default(),
            ));

            // Press: on the thumb it grabs where it was pressed; beside it, the view
            // jumps so the thumb is centred under the pointer and the drag continues.
            {
                let state = state.clone();
                let handle = handle.clone();
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let y = event.position.y;
                    let grab = if geometry.thumb_contains(y) {
                        y - geometry.thumb_top
                    } else {
                        let grab = geometry.thumb_length / 2.0;
                        let offset = handle.offset();
                        handle.set_offset(point(offset.x, geometry.offset_for(y - grab)));
                        grab
                    };
                    state.update(cx, |state, _| state.grab = Some(grab));
                    cx.stop_propagation();
                    window.refresh();
                });
            }
            {
                let state = state.clone();
                let handle = handle.clone();
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let (grab, was_hovered) = {
                        let state = state.read(cx);
                        (state.grab, state.hovered)
                    };
                    if let Some(grab) = grab {
                        if event.pressed_button == Some(MouseButton::Left) {
                            let offset = handle.offset();
                            handle.set_offset(point(
                                offset.x,
                                geometry.offset_for(event.position.y - grab),
                            ));
                            cx.stop_propagation();
                        } else {
                            // The button came up outside the window.
                            state.update(cx, |state, _| state.grab = None);
                        }
                        window.refresh();
                        return;
                    }
                    let hovered = hitbox.is_hovered(window);
                    if hovered != was_hovered {
                        state.update(cx, |state, _| state.hovered = hovered);
                        window.refresh();
                    }
                });
            }
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                if state.read(cx).grab.is_some() {
                    state.update(cx, |state, _| state.grab = None);
                    cx.stop_propagation();
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(Pixels::ZERO)
    .h(Pixels::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.0), px(100.0)), size(px(TRACK_WIDTH), px(height)))
    }

    #[test]
    fn the_thumb_offset_round_trips_from_top_to_bottom() {
        let geometry = Geometry {
            track: track(404.0),
            inset: px(THUMB_INSET),
            thumb_top: px(102.0),
            thumb_length: px(100.0),
            travel: px(300.0),
            max: px(600.0),
        };
        assert_eq!(geometry.offset_for(px(102.0)), px(0.0));
        assert_eq!(geometry.offset_for(px(402.0)), px(-600.0));
        assert_eq!(geometry.offset_for(px(252.0)), px(-300.0));
        assert_eq!(
            geometry.offset_for(px(0.0)),
            px(0.0),
            "a drag above the track stops at the top"
        );
        assert_eq!(
            geometry.offset_for(px(9000.0)),
            px(-600.0),
            "a drag below the track stops at the bottom"
        );
    }

    #[test]
    fn nothing_to_scroll_draws_no_bar() {
        let handle = ScrollHandle::new();
        assert!(Geometry::measure(&handle, track(400.0), theme::Metrics::default()).is_none());
    }

    #[test]
    fn track_width_scales_with_metrics() {
        let regular = theme::Metrics::new(theme::Zoom::Regular);
        let comfortable = theme::Metrics::new(theme::Zoom::Comfortable);
        assert_eq!(track_width(regular), px(TRACK_WIDTH));
        assert!(
            track_width(comfortable) > track_width(regular),
            "scrollbar hit strip widens at larger zoom levels"
        );
    }
}
