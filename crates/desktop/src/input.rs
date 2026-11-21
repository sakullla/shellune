//! UI producers never wait for the network. Only the newest unsent position
//! between two input transitions needs a place in the wire queue.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

struct Pending<T> {
    commands: VecDeque<T>,
    buttons: u32,
    tail_is_motion: bool,
}

pub(crate) struct Sender<T>(Weak<Mutex<Pending<T>>>);
pub(crate) struct Receiver<T>(Arc<Mutex<Pending<T>>>);

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let queue = Arc::new(Mutex::new(Pending {
        commands: VecDeque::new(),
        buttons: 0,
        tail_is_motion: false,
    }));
    (Sender(Arc::downgrade(&queue)), Receiver(queue))
}

impl<T> Sender<T> {
    pub(crate) fn send(&self, command: T, buttons: Option<u32>) -> Result<(), crate::Error> {
        let queue = self.0.upgrade().ok_or(crate::Error::State)?;
        let mut pending = queue.lock().unwrap_or_else(|e| e.into_inner());
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
        Ok(())
    }
}

impl<T> Receiver<T> {
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
    fn stalled_writer_keeps_edges_wheel_and_last_motion_in_order() {
        let (tx, rx) = channel();
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
