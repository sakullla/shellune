//! The window's own title bar.
//!
//! On Windows and macOS the system title bar is hidden and the top row of the window
//! takes its place: the sidebar's header and the page header (or the tab bar). The
//! blank parts of that row drag the window and a double click maximises it, as the
//! system bar did; on Windows the minimise, maximise and close buttons are drawn at its
//! right end and answer through the system, so snap layouts and the keyboard behave as
//! in any other window. Linux keeps the system bar: drawing it there would also mean
//! drawing the window's edges and shadow.

use gpui::prelude::*;
use gpui::{Context, Div, Stateful, Window, WindowControlArea, div, px};

use super::ShelluneView;
use crate::app::icons::{self, Icon};
use crate::app::nav::Platform;
use crate::app::theme::{self, palette};

/// Whether the window draws its own title bar on this platform.
#[must_use]
pub fn custom_titlebar(platform: Platform) -> bool {
    matches!(platform, Platform::Windows | Platform::MacOs)
}

/// The room the macOS window buttons need at the left of the top row.
pub const TRAFFIC_LIGHT_ROOM: f32 = 72.0;

/// Where the macOS window buttons sit, centred in the 52-point header row.
pub const TRAFFIC_LIGHT_POSITION: (f32, f32) = (18.0, 19.0);

/// The background colour of the close button under the pointer, the platform's red.
const CLOSE_HOVER: u32 = 0xc42b1c;

impl ShelluneView {
    /// A blank part of the top row: it drags the window and a double click maximises.
    pub(super) fn drag_area(&self, id: &'static str) -> Stateful<Div> {
        let area = div().id(id).debug_selector(move || id.to_owned());
        match Platform::current() {
            // Windows answers the hit test itself: a press here is a press on the
            // caption, with every system behaviour that comes with it.
            Platform::Windows => area.window_control_area(WindowControlArea::Drag),
            // macOS drags the top of a window natively; the double click is forwarded so
            // it does what the user chose in the system settings.
            Platform::MacOs => area.on_click(|event, window, _cx| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            }),
            Platform::Linux => area,
        }
    }

    /// Minimise, maximise or restore, and close, at the right end of the top row.
    ///
    /// Only drawn on Windows. Each button is a window control area, so the press is the
    /// system's: it minimises, maximises with snap layouts on hover, and closes the way
    /// the caption buttons do.
    pub(super) fn window_controls(&self, window: &Window, _cx: &mut Context<Self>) -> Option<Div> {
        if Platform::current() != Platform::Windows {
            return None;
        }
        let metrics = self.metrics();
        let height = metrics.header_height();
        let button = |id: &'static str, icon: Icon, area: WindowControlArea, close: bool| {
            let hover_bg = if close {
                gpui::rgb(CLOSE_HOVER)
            } else {
                theme::swatch(palette::SURFACE_HOVER)
            };
            let hover_text = if close {
                gpui::rgb(0xffffff)
            } else {
                theme::swatch(palette::TEXT_PRIMARY)
            };
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .group(id)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(px(46.0))
                .h(height)
                .window_control_area(area)
                .hover(move |style| style.bg(hover_bg))
                .child(
                    icons::icon(icon, px(14.0), theme::swatch(palette::TEXT_SECONDARY))
                        .group_hover(id, move |style| style.text_color(hover_text)),
                )
        };
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .debug_selector(|| "window.controls".to_owned())
                .child(button(
                    "window.minimize",
                    Icon::Minimize,
                    WindowControlArea::Min,
                    false,
                ))
                .child(button(
                    "window.maximize",
                    if window.is_maximized() {
                        Icon::Restore
                    } else {
                        Icon::Maximize
                    },
                    WindowControlArea::Max,
                    false,
                ))
                .child(button(
                    "window.close",
                    Icon::Close,
                    WindowControlArea::Close,
                    true,
                )),
        )
    }
}
