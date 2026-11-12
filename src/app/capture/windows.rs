//! The Win32 half of the capture probe: window identity, size and paint scheduling.
//!
//! Pixels no longer come from this module. PrintWindow cannot read a hidden DirectX
//! surface, so the probe reads the rendered frame through the vendored GPUI renderer.
//! Nothing here touches the screen, the desktop window or another application (R1).

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, GetWindowRect, IsWindowVisible, PostMessageW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos, WM_PAINT,
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

pub(super) fn resize_client(handle: Handle, width: i32, height: i32) -> Result<(), String> {
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let mut window = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe {
        if GetClientRect(handle as _, &mut client) == 0
            || GetWindowRect(handle as _, &mut window) == 0
        {
            return Err("the system refused to measure the capture window".to_owned());
        }
        let frame_width = (window.right - window.left) - (client.right - client.left);
        let frame_height = (window.bottom - window.top) - (client.bottom - client.top);
        if SetWindowPos(
            handle as _,
            std::ptr::null_mut(),
            0,
            0,
            width + frame_width,
            height + frame_height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        ) == 0
        {
            return Err("the system refused to resize the capture window".to_owned());
        }
    }
    Ok(())
}

pub(super) struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}
