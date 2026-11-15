//! Bounded terminal output with lossless backpressure and bounded UI batches.
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

const QUEUED_BYTES: usize = 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const BATCH_BYTES: usize = 64 * 1024;
const BATCH_EVENTS: usize = 64;

struct Entry<T> {
    event: T,
    bytes: usize,
    _permit: Option<OwnedSemaphorePermit>,
}

pub(crate) struct Sender<T> {
    tx: mpsc::UnboundedSender<Entry<T>>,
    budget: Arc<Semaphore>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            budget: self.budget.clone(),
        }
    }
}

pub(crate) struct Receiver<T> {
    rx: mpsc::UnboundedReceiver<Entry<T>>,
    budget: Arc<Semaphore>,
}

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let budget = Arc::new(Semaphore::new(QUEUED_BYTES));
    (
        Sender {
            tx,
            budget: budget.clone(),
        },
        Receiver { rx, budget },
    )
}

impl<T> Sender<T> {
    // Control events must remain ordered with output, without waiting for byte space.
    pub(crate) fn send(&self, event: T) -> Result<(), ()> {
        self.tx
            .send(Entry {
                event,
                bytes: 0,
                _permit: None,
            })
            .map_err(|_| ())
    }

    pub(crate) async fn data(&self, data: &[u8], wrap: impl Fn(Vec<u8>) -> T) -> Result<(), ()> {
        for chunk in data.chunks(CHUNK) {
            // Charge small chunks too, so per-message overhead is bounded.
            let permit = self
                .budget
                .clone()
                .acquire_many_owned(chunk.len().max(1024) as u32)
                .await
                .map_err(|_| ())?;
            self.tx
                .send(Entry {
                    event: wrap(chunk.to_vec()),
                    bytes: chunk.len(),
                    _permit: Some(permit),
                })
                .map_err(|_| ())?;
        }
        Ok(())
    }
}

impl<T> Receiver<T> {
    pub(crate) fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.rx.try_recv().map(|entry| entry.event)
    }

    pub(crate) fn drain(&mut self) -> Vec<T> {
        let mut events = Vec::new();
        let mut bytes = 0;
        while events.len() < BATCH_EVENTS && bytes < BATCH_BYTES {
            let Ok(entry) = self.rx.try_recv() else { break };
            bytes += entry.bytes;
            events.push(entry.event);
        }
        events
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.budget.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_consumer_is_bounded_ordered_and_drop_wakes_sender() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (tx, mut rx) = channel::<Vec<u8>>();
                tx.data(&vec![1; QUEUED_BYTES], |bytes| bytes)
                    .await
                    .unwrap();
                assert_eq!(tx.budget.available_permits(), 0);
                let blocked = tx.data(&[2], |bytes| bytes);
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(10), blocked)
                        .await
                        .is_err()
                );
                let batch = rx.drain();
                assert_eq!(batch.iter().map(Vec::len).sum::<usize>(), BATCH_BYTES);
                tx.data(&[2], |bytes| bytes).await.unwrap();
                tx.send(vec![3]).unwrap();
                let mut rest = Vec::new();
                while let Ok(bytes) = rx.try_recv() {
                    rest.extend(bytes);
                }
                assert_eq!(&rest[rest.len() - 2..], &[2, 3]);
                tx.data(&vec![0; QUEUED_BYTES], |bytes| bytes)
                    .await
                    .unwrap();
                let waiting = tokio::spawn(async move { tx.data(&[4], |bytes| bytes).await });
                tokio::task::yield_now().await;
                drop(rx);
                assert!(waiting.await.unwrap().is_err());
            });
    }
}
