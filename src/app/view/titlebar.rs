//! The window's own title bar.
//!
//! On Windows and macOS the system title bar is hidden and the top row of the window
//! takes its place: the sidebar's header and the page header (or the tab bar). The
//! blank parts of that row drag the window and a double click maximises it, as the
//! system bar did; on Windows the minimise, maximise and close buttons are drawn at its
//! right end and answer through the system, so snap layouts and the keyboard behave as
//! in any other window. Linux keeps the system bar: drawing it there would also mean
//! drawing the window's edges and shadow.
//!
//! The macOS traffic lights are the system's, so this module draws none of them. Their
//! red button does not end the application: it hides the window and everything running
//! in it, and the Dock icon brings the same window back (R13, see
//! `ShelluneView::on_window_should_close`).

use gpui::prelude::*;
use gpui::{Context, Div, MouseButton, Stateful, Window, WindowControlArea, div, px};

use super::ShelluneView;
use crate::app::icons::{self, Icon};
use crate::app::nav::Platform;
use crate::app::theme::{self, Metrics, palette, spacing};

/// Whether the window draws its own title bar on this platform.
#[must_use]
pub fn custom_titlebar(platform: Platform) -> bool {
    matches!(platform, Platform::Windows | Platform::MacOs)
}

/// The width of one Windows window button, in pixels. The system scales its own caption
/// buttons with the display, not with the window's text size, so neither does this.
const CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// The room the window buttons this row draws itself take at its right end (R2).
///
/// Only Windows draws them; elsewhere the system's own buttons sit outside the row
/// (Linux) or are allowed for at its left end (macOS, [`traffic_light_room`]).
#[must_use]
pub fn window_controls_width(platform: Platform) -> gpui::Pixels {
    if platform == Platform::Windows {
        px(CAPTION_BUTTON_WIDTH * 3.0)
    } else {
        px(0.0)
    }
}

/// The height of one macOS window button, in points. The system draws the buttons, so
/// they keep this size whatever text size the window uses.
const TRAFFIC_LIGHT_SIZE: f32 = 14.0;

/// The width of the three macOS window buttons and the gaps between them, in points.
const TRAFFIC_LIGHT_SPAN: f32 = 52.0;

/// How far the first macOS window button sits from the window's left edge, in points.
const TRAFFIC_LIGHT_INSET: f32 = 16.0;

/// Where the macOS window buttons sit: inset from the left edge and centred on the top
/// row at the text size `metrics` draws (R2).
///
/// The row grows with the text size and the buttons do not, so the vertical position is
/// worked out from the row's height rather than written down for one size. The system
/// places the buttons when the window opens, so the window asks with the text size it
/// will open at.
#[must_use]
pub fn traffic_light_position(metrics: Metrics) -> (f32, f32) {
    let header = metrics.header_height() / px(1.0);
    let top = ((header - TRAFFIC_LIGHT_SIZE) / 2.0).max(0.0).round();
    (TRAFFIC_LIGHT_INSET, top)
}

/// The room the macOS window buttons need at the left of the top row (R2).
///
/// The buttons are the system's and do not scale, so the room is their own span plus a
/// scaled gap: at a small text size the first tab can no longer slide under them, and at
/// a large one the row does not lose a scaled-up margin it has no use for.
#[must_use]
pub fn traffic_light_room(metrics: Metrics) -> gpui::Pixels {
    px(TRAFFIC_LIGHT_INSET + TRAFFIC_LIGHT_SPAN) + metrics.scale(spacing::TIGHT)
}

impl ShelluneView {
    /// A blank part of the top row: it drags the window and a double click maximises.
    ///
    /// The area occludes what is behind it, so a press here belongs to the caption
    /// and to nothing drawn underneath.
    pub(super) fn drag_area(&self, id: &'static str) -> Stateful<Div> {
        let area = div().id(id).debug_selector(move || id.to_owned()).occlude();
        match Platform::current() {
            // Windows: a press here hands the window to the system's own move loop,
            // so dragging, Aero Snap and moving between screens behave as on any
            // caption. The application starts it itself instead of relying on the
            // system's hit test, which did not reach the caption reliably. A double
            // click maximises or restores, as on the system's caption.
            Platform::Windows => area.on_mouse_down(MouseButton::Left, |event, window, _cx| {
                if event.click_count >= 2 {
                    win32::toggle_maximize(window);
                } else {
                    win32::begin_move(window);
                }
            }),
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
    /// Only drawn on Windows. Each button is a window control area, so hovering the
    /// maximise button still shows the snap layouts, and each also sends its command
    /// on click: it minimises, maximises or restores, and closes the way the caption
    /// buttons do.
    pub(super) fn window_controls(&self, window: &Window, _cx: &mut Context<Self>) -> Option<Div> {
        if Platform::current() != Platform::Windows {
            return None;
        }
        let metrics = self.metrics();
        let height = metrics.header_height();
        let button = |id: &'static str,
                      icon: Icon,
                      area: WindowControlArea,
                      close: bool,
                      run: fn(&mut Window)| {
            // The close button turns the platform's red under the pointer, from the
            // palette's solid danger fill, so it warns before the click; a press sinks
            // one step further on every button, so it is felt and not only tinted.
            let hover_bg = if close {
                theme::swatch(palette::DANGER_SOLID)
            } else {
                theme::swatch(palette::SURFACE_HOVER)
            };
            let pressed_bg = if close {
                theme::faded(theme::swatch(palette::DANGER_SOLID), 0.82)
            } else {
                theme::swatch(palette::SURFACE_PRESSED)
            };
            let hover_text = if close {
                theme::swatch(palette::ON_DANGER)
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
                // The platform's own caption button width: the system scales it with
                // the display, not with the window's text size, so a large text size
                // does not push the buttons out of a small window.
                .w(px(CAPTION_BUTTON_WIDTH))
                .h(height)
                .occlude()
                // The click is handled here and stops here, so the command runs once
                // whether or not the system's own hit test saw the press.
                .window_control_area(area)
                .on_click(move |_event, window, cx| {
                    cx.stop_propagation();
                    run(window);
                })
                .hover(move |style| style.bg(hover_bg))
                .active(move |style| style.bg(pressed_bg))
                .child(
                    icons::icon(icon, px(12.0), theme::swatch(palette::TEXT_SECONDARY))
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
                    win32::minimize,
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
                    win32::toggle_maximize,
                ))
                .child(button(
                    "window.close",
                    Icon::Close,
                    WindowControlArea::Close,
                    true,
                    win32::close,
                )),
        )
    }
}

/// The window commands of the custom title bar, sent to the system.
///
/// Off Windows the caption and buttons are the system's, so these do the closest
/// portable thing or nothing.
mod win32 {
    #[cfg(not(target_os = "windows"))]
    use gpui::Window;

    #[cfg(target_os = "windows")]
    mod imp {
        use gpui::Window;
        use windows_sys::Win32::Foundation::{HWND, POINT};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetCursorPos, HTCAPTION, IsZoomed, PostMessageW, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE,
            SC_RESTORE, WM_NCLBUTTONDOWN, WM_SYSCOMMAND,
        };

        /// None in tests: gpui's test windows have no system window, and asking one
        /// for its handle panics.
        #[cfg(test)]
        fn hwnd(_window: &Window) -> Option<HWND> {
            None
        }

        #[cfg(not(test))]
        fn hwnd(window: &Window) -> Option<HWND> {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            // The trait's handle, not gpui's own `Window::window_handle`.
            match HasWindowHandle::window_handle(window).ok()?.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
                _ => None,
            }
        }

        /// Posted, not sent: the command runs after the event that asked for it.
        fn system_command(window: &Window, command: u32) {
            if let Some(hwnd) = hwnd(window) {
                unsafe {
                    PostMessageW(hwnd, WM_SYSCOMMAND, command as usize, 0);
                }
            }
        }

        /// Turns the press into a press on the caption: the system runs its move loop
        /// from where the pointer is, with snapping and screen changes.
        pub fn begin_move(window: &mut Window) {
            let Some(hwnd) = hwnd(window) else {
                return;
            };
            let mut point = POINT { x: 0, y: 0 };
            unsafe {
                GetCursorPos(&mut point);
                ReleaseCapture();
                let packed = ((point.y as u16 as u32) << 16) | (point.x as u16 as u32);
                PostMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, packed as isize);
            }
        }

        pub fn minimize(window: &mut Window) {
            system_command(window, SC_MINIMIZE);
        }

        pub fn toggle_maximize(window: &mut Window) {
            let zoomed = hwnd(window).is_some_and(|hwnd| unsafe { IsZoomed(hwnd) } != 0);
            system_command(window, if zoomed { SC_RESTORE } else { SC_MAXIMIZE });
        }

        /// The request the system's close button makes, so an open transfer or tunnel
        /// still asks before the window goes.
        pub fn close(window: &mut Window) {
            system_command(window, SC_CLOSE);
        }
    }

    #[cfg(target_os = "windows")]
    pub use imp::{begin_move, close, minimize, toggle_maximize};

    #[cfg(not(target_os = "windows"))]
    pub fn begin_move(_window: &mut Window) {}

    #[cfg(not(target_os = "windows"))]
    pub fn minimize(window: &mut Window) {
        window.minimize_window();
    }

    #[cfg(not(target_os = "windows"))]
    pub fn toggle_maximize(window: &mut Window) {
        window.zoom_window();
    }

    #[cfg(not(target_os = "windows"))]
    pub fn close(_window: &mut Window) {}
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{
        TRAFFIC_LIGHT_INSET, TRAFFIC_LIGHT_SIZE, TRAFFIC_LIGHT_SPAN, traffic_light_position,
        traffic_light_room,
    };
    use crate::app::theme::{Metrics, Zoom};

    #[test]
    fn the_macos_window_buttons_are_centred_on_the_top_row_at_every_text_size() {
        for zoom in Zoom::ALL {
            let metrics = Metrics::new(zoom);
            let header = metrics.header_height() / px(1.0);
            let (x, y) = traffic_light_position(metrics);
            assert_eq!(
                x, TRAFFIC_LIGHT_INSET,
                "the buttons keep their inset at {zoom:?}"
            );
            let above = y;
            let below = header - y - TRAFFIC_LIGHT_SIZE;
            assert!(
                (above - below).abs() <= 1.0,
                "the buttons sit in the middle of the {header}pt row at {zoom:?}: {above} above, {below} below"
            );
        }
    }

    #[test]
    fn the_room_for_the_macos_window_buttons_never_shrinks_under_them() {
        let span = px(TRAFFIC_LIGHT_INSET + TRAFFIC_LIGHT_SPAN);
        for zoom in Zoom::ALL {
            let room = traffic_light_room(Metrics::new(zoom));
            assert!(
                room > span,
                "the first tab starts right of the buttons at {zoom:?}: {room:?} for {span:?}"
            );
            // The buttons do not grow with the text, so the room only grows by its gap.
            assert!(
                room <= span + Metrics::new(zoom).scale(8.0) + px(0.5),
                "the room is the buttons and a gap at {zoom:?}, not a scaled-up margin: {room:?}"
            );
        }
    }
}
