//! UI producers never wait for the network. Only the newest unsent position
//! between two input transitions needs a place in the wire queue.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

struct Pending<T> {
    commands: VecDeque<T>,
    poller: Weak<polling::Poller>,
    accepting: bool,
    buttons: u32,
    tail_is_motion: bool,
}

pub(crate) struct Sender<T> {
    queue: Weak<Mutex<Pending<T>>>,
    awaiting_reconnect_frame: Option<Arc<AtomicBool>>,
}
pub(crate) struct Receiver<T>(Arc<Mutex<Pending<T>>>);

pub(crate) fn channel<T>(
    awaiting_reconnect_frame: Option<Arc<AtomicBool>>,
) -> (Sender<T>, Receiver<T>) {
    let queue = Arc::new(Mutex::new(Pending {
        commands: VecDeque::new(),
        poller: Weak::new(),
        accepting: true,
        buttons: 0,
        tail_is_motion: false,
    }));
    (
        Sender {
            queue: Arc::downgrade(&queue),
            awaiting_reconnect_frame,
        },
        Receiver(queue),
    )
}

impl<T> Sender<T> {
    pub(crate) fn send(&self, command: T, buttons: Option<u32>) -> Result<(), crate::Error> {
        let queue = self.queue.upgrade().ok_or(crate::Error::State)?;
        let mut pending = queue.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.accepting
            || self
                .awaiting_reconnect_frame
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(crate::Error::State);
        }
        let was_empty = pending.commands.is_empty();
        let motion = buttons.is_some_and(|bits| bits == pending.buttons && bits & !7 == 0);
        if motion
            && pending.tail_is_motion
            && let Some(last) = pending.commands.back_mut()
        {
            *last = command;
        } else {
            pending.commands.push_back(command);
        }
        if let Some(bits) = buttons {
            pending.buttons = bits;
        }
        pending.tail_is_motion = motion;
        if was_empty && let Some(poller) = pending.poller.upgrade() {
            drop(pending);
            let _ = poller.notify();
        }
        Ok(())
    }
}

impl<T> Receiver<T> {
    pub(crate) fn wake_on_input(&self, poller: &Arc<polling::Poller>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).poller = Arc::downgrade(poller);
    }

    /// A bounded batch may leave more commands queued while the peer is idle.
    pub(crate) fn wake_if_pending(&self) {
        let pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.commands.is_empty()
            && let Some(poller) = pending.poller.upgrade()
        {
            drop(pending);
            let _ = poller.notify();
        }
    }
    pub(crate) fn disconnect(&self) {
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        pending.accepting = false;
        pending.buttons = 0;
        pending.tail_is_motion = false;
        pending.commands.clear();
    }

    pub(crate) fn resume(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).accepting = true;
    }

    pub(crate) fn pop(&self) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .commands
            .pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn reconnect_drops_old_edges_and_waits_for_the_new_picture() {
        let waiting = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel(Some(waiting.clone()));
        tx.send(1, Some(1)).unwrap();
        rx.disconnect();
        assert!(rx.pop().is_none());
        assert!(tx.send(2, Some(0)).is_err());
        waiting.store(true, Ordering::Release);
        rx.resume();
        assert!(tx.send(3, None).is_err());
        waiting.store(false, Ordering::Release);
        tx.send(4, Some(1)).unwrap();
        assert_eq!(rx.pop(), Some(4));
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn stalled_writer_keeps_edges_wheel_and_last_motion_in_order() {
        let (tx, rx) = channel(None);
        tx.send(0, Some(1)).unwrap();
        for x in 1..10000 {
            tx.send(x, Some(1)).unwrap();
        }
        tx.send(10000, None).unwrap();
        tx.send(10001, Some(1)).unwrap();
        tx.send(10002, Some(0)).unwrap();
        tx.send(10003, Some(8)).unwrap();
        tx.send(10004, Some(8)).unwrap();
        assert_eq!(
            std::iter::from_fn(|| rx.pop()).collect::<Vec<_>>(),
            [0, 9999, 10000, 10001, 10002, 10003, 10004]
        );
        drop(rx);
        assert!(tx.send(0, None).is_err());
    }
}
