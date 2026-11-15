//! A port in memory, for tests that need two ends without a device (R21).
//!
//! R21's acceptance asks for a virtual serial pair. com0com is not installed on this
//! machine and installing a system driver is not something a test may do, so the pair is
//! built one level lower: two [`FakePort`]s joined to each other, each implementing the
//! same [`PortIo`] contract the Windows and Unix backends implement. A session over a fake
//! runs the real reader thread, the real writer thread, the real bounded queue and the
//! real close sequence - only the four system calls are replaced.
//!
//! That distinction is kept honest in the test names and in the task result: a fake pair
//! proves what this crate does with bytes, a queue and a cancellation; it proves nothing
//! about a driver, a UART or a cable, and no test here claims otherwise.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use crate::config::SerialConfig;
use crate::io::{PortFault, PortIo};
use crate::session::{SerialSession, SessionLimits};

/// How a [`FakePort`] behaves.
#[derive(Clone, Debug, Default)]
pub struct FakeSpec {
    /// The most bytes one write call accepts at a time, or `None` for all of them.
    pub write_chunk: Option<usize>,
    /// Writes wait for [`FakePort::release_driver`] instead of being accepted, which is
    /// how a test fills the send queue.
    pub hold_writes: bool,
    /// Writes stop being accepted after this many, so a test can interrupt a frame after
    /// a known number of pieces instead of after a delay.
    pub stall_after_writes: Option<usize>,
    /// A read ignores [`PortIo::interrupt`], as a driver that never answers does.
    pub ignore_interrupt: bool,
    /// A read reports this failure instead of waiting, as an unplugged device does.
    pub read_failure: Option<crate::SerialError>,
    /// Bytes the fake has already received, as if the device had sent them.
    pub scripted: Vec<Vec<u8>>,
}

impl FakeSpec {
    /// A port that accepts and returns everything at once.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts at most `bytes` per write call.
    #[must_use]
    pub fn with_write_chunk(mut self, bytes: usize) -> Self {
        self.write_chunk = Some(bytes);
        self
    }

    /// Holds every write until the driver is released.
    #[must_use]
    pub fn holding_writes(mut self) -> Self {
        self.hold_writes = true;
        self
    }

    /// Accepts exactly `writes` writes and then stops accepting them.
    #[must_use]
    pub fn stalling_after(mut self, writes: usize) -> Self {
        self.stall_after_writes = Some(writes);
        self
    }

    /// Makes a read ignore cancellation, as a stuck driver does.
    #[must_use]
    pub fn ignoring_interrupt(mut self) -> Self {
        self.ignore_interrupt = true;
        self
    }

    /// Makes a read fail with this error, as a device that went away does.
    #[must_use]
    pub fn failing_reads(mut self, error: crate::SerialError) -> Self {
        self.read_failure = Some(error);
        self
    }

    /// Puts bytes in the fake's inbox, as if the device had sent them.
    #[must_use]
    pub fn with_scripted(mut self, chunks: Vec<Vec<u8>>) -> Self {
        self.scripted = chunks;
        self
    }
}

/// One end of an in-memory serial pair.
pub struct FakePort {
    name: String,
    spec: FakeSpec,
    inbox: Mutex<VecDeque<u8>>,
    inbox_wake: Condvar,
    peer: Mutex<Weak<FakePort>>,
    written: Mutex<Vec<u8>>,
    interrupts: AtomicUsize,
    writes: AtomicUsize,
    released: AtomicBool,
    forced: AtomicBool,
    hold: Mutex<()>,
    hold_wake: Condvar,
}

impl FakePort {
    fn new(name: String, spec: FakeSpec) -> Arc<Self> {
        Arc::new(Self {
            name,
            spec: spec.clone(),
            inbox: Mutex::new(spec.scripted.iter().flatten().copied().collect()),
            inbox_wake: Condvar::new(),
            peer: Mutex::new(Weak::new()),
            written: Mutex::new(Vec::new()),
            interrupts: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            released: AtomicBool::new(false),
            forced: AtomicBool::new(false),
            hold: Mutex::new(()),
            hold_wake: Condvar::new(),
        })
    }

    /// Puts bytes in this port's inbox, as if the device had sent them.
    pub fn push_from_device(&self, bytes: &[u8]) {
        let mut inbox = self.lock_inbox();
        inbox.extend(bytes.iter().copied());
        drop(inbox);
        self.inbox_wake.notify_all();
    }

    /// Every byte this port accepted from the session, in order.
    #[must_use]
    pub fn written(&self) -> Vec<u8> {
        lock(&self.written).clone()
    }

    /// How many times the session asked this port to stop.
    #[must_use]
    pub fn interrupts(&self) -> usize {
        self.interrupts.load(Ordering::SeqCst)
    }

    /// Whether the session has released this port.
    #[must_use]
    pub fn is_released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }

    /// Lets a port whose writes are held, or whose read ignores cancellation, finish.
    pub fn release_driver(&self) {
        self.forced.store(true, Ordering::SeqCst);
        self.hold_wake.notify_all();
        self.inbox_wake.notify_all();
    }

    fn lock_inbox(&self) -> MutexGuard<'_, VecDeque<u8>> {
        lock(&self.inbox)
    }

    /// Holds a write until the session gives up or the driver is released.
    fn stall(&self) -> Result<usize, PortFault> {
        let mut guard = lock(&self.hold);
        while !self.forced.load(Ordering::SeqCst) {
            if self.interrupts.load(Ordering::SeqCst) > 0 {
                return Err(PortFault::Interrupted);
            }
            let (next, _) = self
                .hold_wake
                .wait_timeout(guard, Duration::from_millis(2))
                .unwrap_or_else(PoisonError::into_inner);
            guard = next;
        }
        Err(PortFault::Interrupted)
    }
}

impl PortIo for FakePort {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, PortFault> {
        if let Some(error) = &self.spec.read_failure {
            return Err(PortFault::Failed(error.clone()));
        }
        let mut inbox = self.lock_inbox();
        loop {
            if !inbox.is_empty() {
                let take = buf.len().min(inbox.len());
                for slot in buf.iter_mut().take(take) {
                    *slot = inbox.pop_front().unwrap_or(0);
                }
                return Ok(take);
            }
            if self.forced.load(Ordering::SeqCst) {
                return Err(PortFault::Interrupted);
            }
            if self.interrupts.load(Ordering::SeqCst) > 0 && !self.spec.ignore_interrupt {
                return Err(PortFault::Interrupted);
            }
            let (guard, _) = self
                .inbox_wake
                .wait_timeout(inbox, Duration::from_millis(2))
                .unwrap_or_else(PoisonError::into_inner);
            inbox = guard;
        }
    }

    fn write(&self, buf: &[u8]) -> Result<usize, PortFault> {
        let accepted = self.writes.load(Ordering::SeqCst);
        let stalled = self.spec.hold_writes
            || self
                .spec
                .stall_after_writes
                .is_some_and(|limit| accepted >= limit);
        if stalled {
            return self.stall();
        }
        if self.interrupts.load(Ordering::SeqCst) > 0 {
            return Err(PortFault::Interrupted);
        }
        let take = self
            .spec
            .write_chunk
            .map_or(buf.len(), |chunk| chunk.min(buf.len()));
        if take == 0 {
            return Ok(0);
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        lock(&self.written).extend_from_slice(&buf[..take]);
        let peer = lock(&self.peer).upgrade();
        if let Some(peer) = peer {
            peer.push_from_device(&buf[..take]);
        }
        Ok(take)
    }

    fn interrupt(&self) {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        self.hold_wake.notify_all();
        self.inbox_wake.notify_all();
    }

    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }
}

/// A single fake port with the given behaviour.
#[must_use]
pub fn fake(name: &str, spec: FakeSpec) -> Arc<FakePort> {
    FakePort::new(name.to_string(), spec)
}

/// Two fake ports joined to each other: what one writes, the other reads.
#[must_use]
pub fn fake_pair() -> (Arc<FakePort>, Arc<FakePort>) {
    fake_pair_with(FakeSpec::new(), FakeSpec::new())
}

/// Two joined fake ports with their own behaviour.
#[must_use]
pub fn fake_pair_with(left: FakeSpec, right: FakeSpec) -> (Arc<FakePort>, Arc<FakePort>) {
    let first = FakePort::new("fake-a".to_string(), left);
    let second = FakePort::new("fake-b".to_string(), right);
    *lock(&first.peer) = Arc::downgrade(&second);
    *lock(&second.peer) = Arc::downgrade(&first);
    (first, second)
}

/// Runs a real session over a fake port.
///
/// # Panics
///
/// Panics if the session threads cannot start, which on a test machine means the test
/// environment itself is broken.
#[must_use]
pub fn session_over(port: Arc<FakePort>, config: SerialConfig) -> SerialSession {
    session_over_with(port, config, SessionLimits::default())
}

/// Runs a real session over a fake port with the given bounds.
///
/// # Panics
///
/// As [`session_over`].
#[must_use]
pub fn session_over_with(
    port: Arc<FakePort>,
    config: SerialConfig,
    limits: SessionLimits,
) -> SerialSession {
    SerialSession::start(port, config, limits).expect("a fake port always starts a session")
}

/// Two sessions joined into a virtual pair.
#[must_use]
pub fn session_pair(
    left: FakeSpec,
    right: FakeSpec,
    config: SerialConfig,
) -> (SerialSession, SerialSession) {
    let (first, second) = fake_pair_with(left, right);
    (session_over(first, config), session_over(second, config))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
