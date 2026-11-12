//! The Win32 half of the capture probe: read one window's client area back as pixels.
//!
//! Only two things are ever asked of the system here: whether a window is visible, and
//! what its own client area looks like. The screen, the desktop window and every other
//! application are out of reach on purpose — no `GetDC(NULL)`, no BitBlt from a screen
//! DC, no DWM thumbnail (R1).
//!
//! The pixels come from `PrintWindow(hwnd, ..., PW_RENDERFULLCONTENT)`, which asks the
//! window to render itself into a memory DC. A window that has no composited or
//! redirection surface — the normal state of a window that was never shown — answers
//! with nothing useful, and this module reports that as an error instead of inventing a
//! fallback.
//!
//! Handles cross this module as `Handle` values rather than raw `HWND` pointers so that
//! the caller can hold one across an await point without making it `Send`.

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, HGDIOBJ, SelectObject,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, GetWindowRect, IsWindowVisible, PW_RENDERFULLCONTENT,
    PostMessageW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos,
    WM_PAINT,
};

/// A system window handle, carried as a plain number.
pub(super) type Handle = isize;

/// The window that has the keyboard right now, so a probe can prove it did not change.
pub(super) fn foreground() -> Handle {
    unsafe { GetForegroundWindow() as Handle }
}

/// Whether the system considers the window visible to the user.
pub(super) fn visible(handle: Handle) -> bool {
    unsafe { IsWindowVisible(handle as _) != 0 }
}

/// Asks the window to paint on its own message queue.
///
/// Posted, never sent: the paint has to happen after the caller's closure returns, or
/// gpui would draw while it is still borrowing its own state.
pub(super) fn post_paint(handle: Handle) {
    unsafe {
        PostMessageW(handle as _, WM_PAINT, 0, 0);
    }
}

/// The window's client area in physical pixels.
pub(super) fn client_size(handle: Handle) -> Result<(i32, i32), String> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let ok = unsafe { GetClientRect(handle as _, &mut rect) };
    if ok == 0 {
        return Err("the system refused to report the capture window's client area".to_owned());
    }
    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

/// Gives the window a client area of the given physical size, without activating it.
///
/// The size is applied to the window rect, so the difference between the client area and
/// the window rect is added rather than assumed; the caller measures again and repeats if
/// the window has a border. `SWP_NOACTIVATE` matters more than the size: a hidden window
/// can still be activated, and the probe must not take the foreground from the user (R1).
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
        let ok = SetWindowPos(
            handle as _,
            std::ptr::null_mut(),
            0,
            0,
            width + frame_width,
            height + frame_height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
        if ok == 0 {
            return Err("the system refused to resize the capture window".to_owned());
        }
    }
    Ok(())
}

/// One client area read back as tightly packed RGB.
pub(super) struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Renders the window's client area and reads it back.
///
/// `full_content` asks the system to compose the window itself (`PW_RENDERFULLCONTENT`);
/// without it `PrintWindow` sends `WM_PRINTCLIENT` to the window instead. The probe uses
/// both to tell "the window has no surface to render from" apart from "the application
/// does not answer `WM_PRINTCLIENT`".
///
/// The failure text says what the system did, not what a later task might guess: a failed
/// `PrintWindow` is exactly the case this probe exists to detect.
pub(super) fn capture_client(handle: Handle, full_content: bool) -> Result<Image, String> {
    let (width, height) = client_size(handle)?;
    if width <= 0 || height <= 0 {
        return Err(format!(
            "the capture window's client area is {width}x{height}, so there is nothing to read"
        ));
    }
    let width_u32 = width as u32;
    let height_u32 = height as u32;

    unsafe {
        let memory = CreateCompatibleDC(std::ptr::null_mut());
        if memory.is_null() {
            return Err("the system refused to create a memory device context".to_owned());
        }
        let bitmap = CreateCompatibleBitmap(memory, width, height);
        if bitmap.is_null() {
            DeleteDC(memory);
            return Err("the system refused to create the capture bitmap".to_owned());
        }
        let previous = SelectObject(memory, bitmap as HGDIOBJ);

        let flags = if full_content {
            PW_RENDERFULLCONTENT
        } else {
            0
        };
        let rendered = PrintWindow(handle as _, memory, flags);
        if rendered == 0 {
            SelectObject(memory, previous);
            DeleteObject(bitmap as HGDIOBJ);
            DeleteDC(memory);
            return Err(format!(
                "PrintWindow({flags}) failed: the window has no surface the system can render \
                 from (a hidden window that was never shown usually has none)"
            ));
        }

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: the rows come back top-down, the order a PNG wants.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                // BI_RGB with 32 bits is BGRA with the alpha byte unused.
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default(); 1],
        };
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        let lines = GetDIBits(
            memory,
            bitmap,
            0,
            height_u32,
            bgra.as_mut_ptr() as *mut std::ffi::c_void,
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(memory, previous);
        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(memory);

        if lines == 0 {
            return Err(
                "GetDIBits returned no scan lines, so the window rendered nothing".to_owned(),
            );
        }

        let (pixels, _) = bgra.as_chunks::<4>();
        let mut rgb = Vec::with_capacity(pixels.len() * 3);
        for pixel in pixels {
            // BGRA in, RGB out; the alpha byte is unused by BI_RGB.
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        Ok(Image {
            width: width_u32,
            height: height_u32,
            rgb,
        })
    }
}
