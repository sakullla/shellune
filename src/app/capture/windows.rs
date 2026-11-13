//! The Win32 half of the capture probe: window identity, size and paint scheduling.
//!
//! Pixels no longer come from this module. PrintWindow cannot read a hidden DirectX
//! surface, so the probe reads the rendered frame through the vendored GPUI renderer.
//! Nothing here touches the screen, the desktop window or another application (R1).

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, IsWindowVisible, PostMessageW, WM_PAINT,
};

pub(super) type Handle = isize;

pub(super) fn foreground() -> Handle {
    unsafe { GetForegroundWindow() as Handle }
}

pub(super) fn visible(handle: Handle) -> bool {
    unsafe { IsWindowVisible(handle as _) != 0 }
}

pub(super) fn post_paint(handle: Handle) {
    unsafe {
        PostMessageW(handle as _, WM_PAINT, 0, 0);
    }
}

pub(super) fn client_size(handle: Handle) -> Result<(i32, i32), String> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetClientRect(handle as _, &mut rect) } == 0 {
        return Err("the system refused to report the capture window's client area".to_owned());
    }
    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

pub(super) struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}
