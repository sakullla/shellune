//! AppKit tracks bitmap cursors independently of window painting.
use super::ns_string;
use crate::{CursorStyle, RenderImage, Window};
use cocoa::{
    base::{NO, YES, id, nil},
    foundation::{NSPoint, NSSize},
};
use objc::{class, msg_send, rc::StrongPtr, sel, sel_impl};
use std::{
    cell::RefCell,
    collections::HashMap,
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};

thread_local! {
    static CURSORS: RefCell<HashMap<usize,StrongPtr>> = RefCell::default();
}

/// Main-thread handle keeping an AppKit cursor available to hit-tested elements.
pub struct BitmapCursor {
    key: usize,
    _main_thread: PhantomData<Rc<()>>,
}

impl BitmapCursor {
    /// Cursor style valid for the lifetime of this handle.
    pub fn style(&self) -> CursorStyle {
        CursorStyle::Bitmap(self.key)
    }
}

impl Drop for BitmapCursor {
    fn drop(&mut self) {
        CURSORS.with_borrow_mut(|cursors| cursors.remove(&self.key));
    }
}

pub(super) fn lookup(key: usize) -> Option<id> {
    CURSORS.with_borrow(|cursors| cursors.get(&key).map(|cursor| **cursor))
}

impl Window {
    /// Create a native cursor from a straight-alpha BGRA image. AppKit measures
    /// the hotspot and cursor size in logical points.
    pub fn bitmap_cursor(
        &mut self,
        image: &RenderImage,
        hotspot: (u16, u16),
        points_per_pixel: f32,
    ) -> Option<BitmapCursor> {
        #[cfg(any(test, feature = "test-support"))]
        if self.platform_window.as_test().is_some() {
            return None;
        }
        if image.frame_count() == 0 {
            return None;
        }
        let size = image.size(0);
        let (w, h) = (size.width.0 as usize, size.height.0 as usize);
        let source = image.as_bytes(0)?;
        if w == 0
            || h == 0
            || !points_per_pixel.is_finite()
            || points_per_pixel <= 0.0
            || usize::from(hotspot.0) >= w
            || usize::from(hotspot.1) >= h
        {
            return None;
        }
        let bytes = w.checked_mul(h)?.checked_mul(4)?;
        if source.len() != bytes {
            return None;
        }
        let scale = f64::from(points_per_pixel);
        unsafe {
            let rep: id = msg_send![class!(NSBitmapImageRep), alloc];
            let rep = StrongPtr::new(
                msg_send![rep,initWithBitmapDataPlanes:std::ptr::null_mut::<*mut u8>()
                pixelsWide:w as isize pixelsHigh:h as isize bitsPerSample:8isize samplesPerPixel:4isize
                hasAlpha:YES isPlanar:NO colorSpaceName:ns_string("NSDeviceRGBColorSpace")
                bitmapFormat:2usize bytesPerRow:(w*4) as isize bitsPerPixel:32isize],
            );
            if rep.is_null() {
                return None;
            }
            let data: *mut u8 = msg_send![*rep, bitmapData];
            if data.is_null() {
                return None;
            }
            for (rgba, bgra) in std::slice::from_raw_parts_mut(data, bytes)
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(source.as_chunks::<4>().0)
            {
                *rgba = [bgra[2], bgra[1], bgra[0], bgra[3]];
            }
            let points = NSSize::new(w as f64 * scale, h as f64 * scale);
            let _: () = msg_send![*rep,setSize:points];
            let img: id = msg_send![class!(NSImage), alloc];
            let img = StrongPtr::new(msg_send![img,initWithSize:points]);
            if img.is_null() {
                return None;
            }
            let _: () = msg_send![*img,addRepresentation:*rep];
            let cursor: id = msg_send![class!(NSCursor), alloc];
            let cursor: id = msg_send![cursor,initWithImage:*img hotSpot:NSPoint::new(f64::from(hotspot.0)*scale,f64::from(hotspot.1)*scale)];
            if cursor == nil {
                return None;
            }
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let key = NEXT.fetch_add(1, Ordering::Relaxed);
            CURSORS.with_borrow_mut(|cursors| cursors.insert(key, StrongPtr::new(cursor)));
            Some(BitmapCursor {
                key,
                _main_thread: PhantomData,
            })
        }
    }
}
