//! Nonblocking callback mailbox. Pending rectangles are composited, not discarded:
//! a slow UI receives the latest pixels without accumulating an unlimited video.
use std::collections::VecDeque;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::{Event, MAX_FRAME_BYTES};

const MAX_NOTIFICATIONS: usize = 128;

#[derive(Default)]
struct Pending {
    // None is the latest frame's place in the event sequence.
    events: VecDeque<Option<Event>>,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    dirty: Option<(usize, usize, usize, usize)>,
}

#[derive(Default)]
pub(crate) struct Mailbox {
    pending: Mutex<Pending>,
    ready: Condvar,
}

impl Mailbox {
    pub(crate) fn deliver(&self, event: Event) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            Event::Frame {
                x,
                y,
                width,
                height,
                desktop_width,
                desktop_height,
                stride,
                pixels,
            } => {
                if !pending.apply(
                    x,
                    y,
                    width,
                    height,
                    desktop_width,
                    desktop_height,
                    stride,
                    &pixels,
                ) {
                    return;
                }
                pending.events.retain(Option::is_some);
                pending.events.push_back(None);
            }
            event => {
                if matches!(
                    event,
                    Event::State {
                        state: crate::State::Connecting,
                        ..
                    }
                ) {
                    pending.events.retain(|event| {
                        matches!(
                            event,
                            Some(
                                Event::State { .. }
                                    | Event::Clipboard { .. }
                                    | Event::Notice { .. }
                            )
                        )
                    });
                    pending.pixels.clear();
                    pending.width = 0;
                    pending.height = 0;
                    pending.dirty = None;
                }
                // Only the newest unconsumed clipboard value is useful.
                if matches!(event, Event::Clipboard { .. }) {
                    pending
                        .events
                        .retain(|e| !matches!(e, Some(Event::Clipboard { .. })));
                }
                if matches!(event, Event::Certificate { .. }) {
                    pending
                        .events
                        .retain(|e| !matches!(e, Some(Event::Certificate { .. })));
                }
                pending.events.push_back(Some(event));
            }
        }
        // State churn must not grow metadata forever either. Never evict a live
        // certificate question or the frame marker; replace the oldest notice.
        if pending.events.len() > MAX_NOTIFICATIONS
            && let Some(index) = pending.events.iter().position(|e| {
                matches!(
                    e,
                    Some(Event::State { .. } | Event::Clipboard { .. } | Event::Notice { .. })
                )
            })
        {
            pending.events.remove(index);
        }
        drop(pending);
        self.ready.notify_one();
    }

    pub(crate) fn try_recv(&self) -> Result<Event, TryRecvError> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .ok_or(TryRecvError::Empty)
    }

    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        let start = Instant::now();
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(event) = pending.pop() {
                return Ok(event);
            }
            let Some(left) = timeout.checked_sub(start.elapsed()) else {
                return Err(RecvTimeoutError::Timeout);
            };
            let (next, _) = self
                .ready
                .wait_timeout(pending, left)
                .unwrap_or_else(|e| e.into_inner());
            pending = next;
        }
    }

    pub(crate) fn drain(&self) -> Vec<Event> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut events = Vec::with_capacity(pending.events.len());
        while let Some(event) = pending.pop() {
            events.push(event);
        }
        events
    }
}

impl Pending {
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        dw: u32,
        dh: u32,
        stride: u32,
        pixels: &[u8],
    ) -> bool {
        if x < 0 || y < 0 || width <= 0 || height <= 0 || dw == 0 || dh == 0 {
            return false;
        }
        let (x, y, w, h) = (x as usize, y as usize, width as usize, height as usize);
        let Some(bytes) = u64::from(dw)
            .checked_mul(u64::from(dh))
            .and_then(|n| n.checked_mul(4))
        else {
            return false;
        };
        if bytes > MAX_FRAME_BYTES as u64
            || x + w > dw as usize
            || y + h > dh as usize
            || (stride as usize) < w * 4
            || (h - 1) * stride as usize + w * 4 > pixels.len()
        {
            return false;
        }
        if (self.width, self.height) != (dw, dh) {
            self.width = dw;
            self.height = dh;
            self.pixels = vec![0; bytes as usize];
            self.dirty = None;
        }
        for row in 0..h {
            let dst = ((y + row) * dw as usize + x) * 4;
            let src = row * stride as usize;
            self.pixels[dst..dst + w * 4].copy_from_slice(&pixels[src..src + w * 4]);
        }
        self.dirty = Some(match self.dirty {
            Some((left, top, right, bottom)) => {
                (left.min(x), top.min(y), right.max(x + w), bottom.max(y + h))
            }
            None => (x, y, x + w, y + h),
        });
        true
    }

    fn pop(&mut self) -> Option<Event> {
        match self.events.pop_front()? {
            Some(event) => {
                if self.dirty.is_none()
                    && matches!(
                        event,
                        Event::State {
                            state: crate::State::Closed | crate::State::Failed(_),
                            ..
                        }
                    )
                {
                    self.pixels = Vec::new();
                    self.width = 0;
                    self.height = 0;
                }
                Some(event)
            }
            None => {
                let (x, y, right, bottom) = self.dirty.take()?;
                let (w, h) = (right - x, bottom - y);
                let mut pixels = Vec::with_capacity(w * h * 4);
                for row in y..bottom {
                    let start = (row * self.width as usize + x) * 4;
                    pixels.extend_from_slice(&self.pixels[start..start + w * 4]);
                }
                Some(Event::Frame {
                    x: x as i32,
                    y: y as i32,
                    width: w as i32,
                    height: h as i32,
                    desktop_width: self.width,
                    desktop_height: self.height,
                    stride: (w * 4) as u32,
                    pixels,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pixel(x: i32, value: u8) -> Event {
        Event::Frame {
            x,
            y: 0,
            width: 1,
            height: 1,
            desktop_width: 4,
            desktop_height: 1,
            stride: 4,
            pixels: vec![value; 4],
        }
    }
    #[test]
    fn slow_consumer_keeps_latest_pixels_with_constant_storage() {
        let queue = Mailbox::default();
        for i in 0..100_000 {
            queue.deliver(pixel(i % 4, (i % 251) as u8));
        }
        let pending = queue.pending.lock().unwrap();
        assert_eq!(pending.events.len(), 1);
        assert_eq!(pending.pixels.len(), 16);
        drop(pending);
        let Event::Frame { pixels, .. } = queue.try_recv().unwrap() else {
            panic!("frame");
        };
        for (index, chunk) in pixels.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(chunk, &[((99_996 + index) % 251) as u8; 4]);
        }
    }
    #[test]
    fn coalescing_preserves_unchanged_pixels_between_dirty_regions() {
        let queue = Mailbox::default();
        queue.deliver(pixel(1, 9));
        queue.try_recv().unwrap();
        queue.deliver(pixel(0, 2));
        queue.deliver(Event::Clipboard {
            entries: vec![crate::ClipboardEntry::text("last")],
        });
        queue.deliver(pixel(3, 7));
        assert!(matches!(queue.try_recv(), Ok(Event::Clipboard { .. })));
        let Event::Frame { pixels, .. } = queue.try_recv().unwrap() else {
            panic!("frame");
        };
        assert_eq!(
            pixels,
            [vec![2; 4], vec![9; 4], vec![0; 4], vec![7; 4]].concat()
        );
        assert!(matches!(
            queue.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn oversized_dimensions_and_notification_floods_are_bounded() {
        let queue = Mailbox::default();
        queue.deliver(Event::Frame {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            desktop_width: u32::MAX,
            desktop_height: u32::MAX,
            stride: 4,
            pixels: vec![0; 4],
        });
        assert!(queue.pending.lock().unwrap().pixels.is_empty());
        for _ in 0..1000 {
            queue.deliver(Event::State {
                state: crate::State::Connected,
                failure: crate::Failure::None,
                detail: 0,
                reason: String::new(),
            });
        }
        assert_eq!(queue.drain().len(), MAX_NOTIFICATIONS);
    }

    #[test]
    fn terminal_state_releases_the_compositing_canvas() {
        let queue = Mailbox::default();
        queue.deliver(pixel(0, 3));
        queue.deliver(Event::State {
            state: crate::State::Closed,
            failure: crate::Failure::None,
            detail: 0,
            reason: String::new(),
        });
        let events = queue.drain();
        assert!(matches!(events[0], Event::Frame { .. }));
        assert!(matches!(
            events[1],
            Event::State {
                state: crate::State::Closed,
                ..
            }
        ));
        assert_eq!(queue.pending.lock().unwrap().pixels.capacity(), 0);
    }
}
