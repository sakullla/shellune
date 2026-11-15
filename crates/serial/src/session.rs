//! One serial session: a reader, a writer, a bounded queue and a close that stops I/O
//! first (R17, R21).
//!
//! The shape is deliberate, and each part answers one line of the recipe and one line of
//! R17/R21:
//!
//! * **two threads, no polling of the device from the caller.** One thread waits for
//!   bytes the device sends, one takes frames the user queued. Neither ever blocks the
//!   application, and the application only reads [`SerialSession::poll`].
//! * **a bounded send queue that refuses, it does not drop.** [`SerialSession::send`]
//!   returns [`crate::SerialCause::QueueFull`] when the queue is at its limit; the bytes
//!   that were accepted stay queued in order. Nothing is ever discarded to make room.
//! * **partial writes are reported, not replayed.** A driver may accept half a frame; the
//!   writer keeps offering the rest of *that* frame and emits one
//!   [`SerialEvent::Sent`] per accepted piece. If the session closes mid-frame, the
//!   remainder is not written later and not written twice: the frame is closed with
//!   [`SerialEvent::SendIncomplete`] saying how much went out, which is what "no
//!   automatic replay" means for a byte stream that cannot be rewound.
//! * **receive backpressure instead of a silent loss.** When the caller has not polled
//!   for a while, the reader stops reading the device rather than growing an unbounded
//!   buffer or dropping bytes; the device's own buffer fills and flow control or the
//!   driver's queue hold the rest.
//! * **a close that stops in-flight I/O before releasing anything.** [`SerialSession::close`]
//!   asks the port to cancel, waits for both threads to finish, and only then drops the
//!   handle. If the driver never completes the cancelled operation, the handle is *not*
//!   released and the session stays in [`SessionState::Closing`] - it never reports a
//!   port as free that is still held, because the next open of the same port would then
//!   fail for a reason the user cannot see.
//!
//! Bytes are bytes: the reader hands the terminal exactly what the device sent, including
//! `0x00` and sequences that are not valid UTF-8. Turning them into text is the
//! terminal's job, not this crate's.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::SerialConfig;
use crate::error::{SerialCause, SerialError};
use crate::io::{PortFault, PortIo};
use crate::port::open_port;

/// How many unsent frames the queue holds by default.
pub const DEFAULT_QUEUED_FRAMES: usize = 256;

/// How many unsent bytes the queue holds by default.
pub const DEFAULT_QUEUED_BYTES: usize = 1024 * 1024;

/// How many received bytes may wait to be polled before the reader stops reading.
pub const DEFAULT_PENDING_RX_BYTES: usize = 1024 * 1024;

/// How long [`SerialSession::close`] waits for the two threads by default.
pub const DEFAULT_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The largest read one call asks the port for.
const READ_CHUNK: usize = 4096;

/// How often a waiting loop re-checks the session state.
const WAIT_TICK: Duration = Duration::from_millis(5);

/// The bounds one session runs under.
///
/// The application uses [`SessionLimits::default`]; the tests use small numbers to reach
/// a limit without writing a megabyte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLimits {
    /// How many unsent frames may be queued.
    pub queued_frames: usize,
    /// How many unsent bytes may be queued, including the frame being written.
    pub queued_bytes: usize,
    /// How many received bytes may wait for a poll.
    pub pending_rx_bytes: usize,
    /// How long a close waits for the port to stop its in-flight operation.
    pub close_timeout: Duration,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            queued_frames: DEFAULT_QUEUED_FRAMES,
            queued_bytes: DEFAULT_QUEUED_BYTES,
            pending_rx_bytes: DEFAULT_PENDING_RX_BYTES,
            close_timeout: DEFAULT_CLOSE_TIMEOUT,
        }
    }
}

impl SessionLimits {
    /// The same limits with every bound at least one, so a zero cannot deadlock a wait.
    fn sane(self) -> Self {
        Self {
            queued_frames: self.queued_frames.max(1),
            queued_bytes: self.queued_bytes.max(1),
            pending_rx_bytes: self.pending_rx_bytes.max(1),
            close_timeout: self.close_timeout,
        }
    }
}

/// Where a session is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// The port is open and both threads are running.
    Open,
    /// A close was asked for and the port has not finished stopping its I/O.
    Closing,
    /// The port was released; nothing in flight, nothing owned.
    Closed,
    /// The device failed or went away; the port was released.
    Failed,
}

impl SessionState {
    /// A label for a status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "disconnected",
        }
    }

    /// Whether the session owns nothing any more.
    #[must_use]
    pub fn is_final(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}

/// Something that happened on the session, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialEvent {
    /// Bytes exactly as the device sent them.
    Received(Vec<u8>),
    /// One piece of a queued frame the port accepted.
    Sent {
        /// The frame this piece belongs to, as [`SerialSession::send`] returned it.
        frame: u64,
        /// How many bytes of that frame this piece carried.
        bytes: usize,
    },
    /// A frame stopped before the port accepted all of it; the rest is not replayed.
    SendIncomplete {
        /// The frame that stopped.
        frame: u64,
        /// How many of its bytes the port accepted.
        sent: usize,
        /// How many bytes the frame held.
        requested: usize,
    },
    /// The session ended because the port failed.
    Disconnected {
        /// Why, with the cause the system allowed and its own code.
        error: SerialError,
    },
    /// The port was released.
    Closed,
    /// The driver dropped bytes it had already received (R21).
    ///
    /// This is the one loss a serial port suffers without any call failing: the
    /// device kept sending while the driver's buffer was full. It is reported as its
    /// own event so the session above this crate can say a gap is a gap instead of
    /// presenting it as a quiet line. Windows reports it; a platform that cannot say
    /// never produces this event, rather than producing a placeholder.
    ReceiveOverflow {
        /// What the driver said, with its own code.
        notice: crate::io::PortNotice,
    },
}

/// What a close achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The port stopped its I/O and was released. Any frames still queued were refused,
    /// and their count is here so nothing disappears without a number.
    Closed {
        /// How many queued frames were never written.
        discarded_frames: usize,
        /// How many bytes those frames held.
        discarded_bytes: usize,
    },
    /// The session was already closed or had failed; nothing was owned any more.
    AlreadyClosed,
    /// The driver did not finish the cancelled operation in time. The session keeps the
    /// port open and stays in [`SessionState::Closing`]; call [`SerialSession::close`]
    /// again once the driver answers.
    StillClosing {
        /// How long this call waited.
        waited: Duration,
    },
}

/// One opened serial port with its reader, writer and queues.
pub struct SerialSession {
    port: Option<Arc<dyn PortIo>>,
    name: String,
    config: SerialConfig,
    limits: SessionLimits,
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    state: SessionState,
}

impl SerialSession {
    /// Opens a port and starts its session.
    ///
    /// # Errors
    ///
    /// The settings are validated first, then the cause the system allowed: a missing
    /// port, a port another program holds, a missing permission, settings the driver
    /// rejects, a device that is gone, or an input/output failure.
    pub fn open(port: &str, config: &SerialConfig) -> Result<Self, SerialError> {
        let port = open_port(port, config)?;
        Self::start(port, *config, SessionLimits::default())
    }

    /// Starts a session over an already opened port.
    pub(crate) fn start(
        port: Arc<dyn PortIo>,
        config: SerialConfig,
        limits: SessionLimits,
    ) -> Result<Self, SerialError> {
        let limits = limits.sane();
        let shared = Arc::new(Shared::new(limits));
        let reader = spawn("shellune-serial-reader", {
            let port = Arc::clone(&port);
            let shared = Arc::clone(&shared);
            move || read_loop(&port, &shared)
        })?;
        let writer = match spawn("shellune-serial-writer", {
            let port = Arc::clone(&port);
            let shared = Arc::clone(&shared);
            move || write_loop(&port, &shared)
        }) {
            Ok(writer) => writer,
            Err(error) => {
                shared.closing.store(true, Ordering::SeqCst);
                shared.wake_everyone();
                port.interrupt();
                let _ = reader.join();
                return Err(error);
            }
        };
        Ok(Self {
            name: port.name().to_string(),
            port: Some(port),
            config,
            limits,
            shared,
            reader: Some(reader),
            writer: Some(writer),
            state: SessionState::Open,
        })
    }

    /// The port name the user knows.
    #[must_use]
    pub fn port_name(&self) -> &str {
        &self.name
    }

    /// The settings the port was opened with.
    #[must_use]
    pub fn config(&self) -> &SerialConfig {
        &self.config
    }

    /// Where the session is in its life.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The bounds this session runs under.
    #[must_use]
    pub fn limits(&self) -> SessionLimits {
        self.limits
    }

    /// Queues bytes for the port and returns the frame number they were queued as.
    ///
    /// The frame is written in order behind everything queued before it. Bytes the port
    /// has accepted are never offered again; if the port accepts only part of the frame,
    /// the rest of *that* frame is offered next, and the events report each accepted
    /// piece - so a caller can tell "sent in pieces" from "sent again".
    ///
    /// # Errors
    ///
    /// [`crate::SerialCause::QueueFull`] when the queue is at its limit - the bytes are
    /// refused, not dropped - and [`crate::SerialCause::Closed`] when the session is
    /// no longer open. An empty frame is refused as [`crate::SerialCause::Refused`].
    pub fn send(&self, bytes: &[u8]) -> Result<u64, SerialError> {
        self.check_open()?;
        if bytes.is_empty() {
            return Err(SerialError::new(SerialCause::Refused)
                .at(self.name.clone())
                .with_detail("an empty frame has nothing to write"));
        }
        self.shared.enqueue(bytes, None)
    }

    /// Queues bytes, waiting up to `timeout` for room in the queue.
    ///
    /// # Errors
    ///
    /// The same causes as [`SerialSession::send`], with
    /// [`crate::SerialCause::QueueFull`] reported if the wait ran out.
    pub fn send_blocking(&self, bytes: &[u8], timeout: Duration) -> Result<u64, SerialError> {
        self.check_open()?;
        if bytes.is_empty() {
            return Err(SerialError::new(SerialCause::Refused)
                .at(self.name.clone())
                .with_detail("an empty frame has nothing to write"));
        }
        self.shared.enqueue(bytes, Some(timeout))
    }

    /// Returns everything that happened since the last poll, in order.
    ///
    /// Polling also lets the reader continue when its received bytes were waiting for
    /// room, and settles the state when a thread has finished.
    pub fn poll(&mut self) -> Vec<SerialEvent> {
        let events = self.shared.take_events();
        self.refresh();
        events
    }

    /// How many frames are queued and not yet started.
    #[must_use]
    pub fn pending_frames(&self) -> usize {
        let state = lock(&self.shared.send);
        state.frames.len()
    }

    /// How many bytes are queued or being written right now.
    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        let state = lock(&self.shared.send);
        state.bytes
    }

    /// Waits until the queue is empty and the last frame has been written.
    ///
    /// Returns whether that happened inside `timeout`; a caller that closes after a
    /// `false` is choosing to drop what is left, and the close reports the count.
    pub fn drain(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.shared.send);
        loop {
            if state.frames.is_empty() && state.in_flight == 0 {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let slice = (deadline - now).min(WAIT_TICK);
            let (guard, _) = self
                .shared
                .send_room
                .wait_timeout(state, slice)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
    }

    /// Stops the in-flight operation, waits for both threads, and releases the port.
    ///
    /// The order matters: the port is asked to cancel first, the two threads are joined
    /// after that, and the handle is dropped only when nothing can still be inside a
    /// system call. A driver that never answers leaves the session in
    /// [`SessionState::Closing`] with the port still owned, and a later call tries again.
    pub fn close(&mut self) -> CloseOutcome {
        match self.state {
            SessionState::Closed | SessionState::Failed => return CloseOutcome::AlreadyClosed,
            SessionState::Open => {
                self.state = SessionState::Closing;
                self.begin_close();
            }
            SessionState::Closing => {}
        }
        let waited = self.wait_for_threads();
        if !self.threads_finished() {
            return CloseOutcome::StillClosing { waited };
        }
        let (discarded_frames, discarded_bytes) = self.finish_close();
        CloseOutcome::Closed {
            discarded_frames,
            discarded_bytes,
        }
    }

    /// Asks for the close without waiting: the next [`SerialSession::poll`] settles it.
    pub fn begin_close(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        self.shared.wake_everyone();
        if let Some(port) = &self.port {
            port.interrupt();
        }
    }

    /// Settles a state whose threads have finished.
    fn refresh(&mut self) {
        if self.threads_finished() {
            if self.shared.failed.load(Ordering::SeqCst) {
                if self.state == SessionState::Open || self.state == SessionState::Closing {
                    self.finish_close();
                    self.state = SessionState::Failed;
                }
            } else if self.state == SessionState::Closing {
                self.finish_close();
            }
        }
    }

    /// Releases the port and reports where that left the session.
    fn finish_close(&mut self) -> (usize, usize) {
        self.join_threads();
        let discarded = self.shared.take_discarded();
        if let Some(port) = self.port.take() {
            port.release();
        }
        if !matches!(self.state, SessionState::Failed) {
            self.state = SessionState::Closed;
        }
        self.shared.push_event(SerialEvent::Closed);
        discarded
    }

    fn wait_for_threads(&mut self) -> Duration {
        let start = Instant::now();
        while !self.threads_finished() {
            if start.elapsed() >= self.limits.close_timeout {
                break;
            }
            thread::sleep(WAIT_TICK.min(Duration::from_millis(2)));
        }
        start.elapsed()
    }

    fn threads_finished(&self) -> bool {
        let live =
            |handle: &Option<JoinHandle<()>>| handle.as_ref().is_some_and(|h| !h.is_finished());
        !live(&self.reader) && !live(&self.writer)
    }

    fn join_threads(&mut self) {
        if self.reader.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = self.reader.take()
        {
            let _ = handle.join();
        }
        if self.writer.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = self.writer.take()
        {
            let _ = handle.join();
        }
    }

    fn check_open(&self) -> Result<(), SerialError> {
        match self.state {
            SessionState::Open => Ok(()),
            SessionState::Closing | SessionState::Closed => {
                Err(SerialError::closed().at(self.name.clone()))
            }
            SessionState::Failed => Err(SerialError::new(SerialCause::Disconnected)
                .at(self.name.clone())
                .with_detail("the device is gone")),
        }
    }
}

impl fmt::Debug for SerialSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialSession")
            .field("port", &self.name)
            .field("config", &self.config)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Drop for SerialSession {
    fn drop(&mut self) {
        if matches!(self.state, SessionState::Open | SessionState::Closing) {
            self.begin_close();
            let _ = self.wait_for_threads();
        }
    }
}

/// What both threads and the caller share.
struct Shared {
    limits: SessionLimits,
    events: Mutex<EventLog>,
    rx_room: Condvar,
    send: Mutex<SendState>,
    send_room: Condvar,
    closing: AtomicBool,
    failed: AtomicBool,
}

struct EventLog {
    events: VecDeque<SerialEvent>,
    pending_bytes: usize,
}

struct SendState {
    frames: VecDeque<Frame>,
    /// Queued bytes plus the frame being written, so the limit covers both.
    bytes: usize,
    in_flight: usize,
    next_id: u64,
}

struct Frame {
    id: u64,
    bytes: Vec<u8>,
}

impl Shared {
    fn new(limits: SessionLimits) -> Self {
        Self {
            limits,
            events: Mutex::new(EventLog {
                events: VecDeque::new(),
                pending_bytes: 0,
            }),
            rx_room: Condvar::new(),
            send: Mutex::new(SendState {
                frames: VecDeque::new(),
                bytes: 0,
                in_flight: 0,
                next_id: 0,
            }),
            send_room: Condvar::new(),
            closing: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }

    /// Hands received bytes to the caller, waiting while there is no room for them.
    ///
    /// Waiting, not dropping, is the whole point: a device that sends faster than the
    /// terminal draws must slow down, and the bytes already received must not vanish.
    fn deliver(&self, data: &[u8]) {
        let mut log = lock(&self.events);
        // The wait is for room for *more* than is already waiting. A single read larger
        // than the whole budget is still accepted when nothing is pending, because
        // refusing it could never make room: the limit bounds what waits for a poll, it
        // does not split a chunk the driver already handed over.
        while log.pending_bytes > 0 && log.pending_bytes + data.len() > self.limits.pending_rx_bytes
        {
            if self.closing.load(Ordering::SeqCst) {
                return;
            }
            log = self
                .rx_room
                .wait(log)
                .unwrap_or_else(PoisonError::into_inner);
        }
        log.pending_bytes += data.len();
        log.events.push_back(SerialEvent::Received(data.to_vec()));
    }

    fn push_event(&self, event: SerialEvent) {
        let mut log = lock(&self.events);
        log.events.push_back(event);
    }

    fn take_events(&self) -> Vec<SerialEvent> {
        let mut log = lock(&self.events);
        let mut taken = Vec::with_capacity(log.events.len());
        while let Some(event) = log.events.pop_front() {
            if let SerialEvent::Received(data) = &event {
                log.pending_bytes = log.pending_bytes.saturating_sub(data.len());
            }
            taken.push(event);
        }
        drop(log);
        self.rx_room.notify_all();
        taken
    }

    /// Records that the port failed and stops both threads from waiting for more.
    fn note_failure(&self, error: SerialError) {
        self.failed.store(true, Ordering::SeqCst);
        self.closing.store(true, Ordering::SeqCst);
        self.push_event(SerialEvent::Disconnected { error });
        self.wake_everyone();
    }

    fn wake_everyone(&self) {
        // Closing is atomic, but waiters inspect it while holding these locks.
        // Acquire each lock before notifying so close cannot land between their
        // predicate check and wait. Idle workers then need no periodic polling.
        {
            let _events = lock(&self.events);
            self.rx_room.notify_all();
        }
        {
            let _send = lock(&self.send);
            self.send_room.notify_all();
        }
    }

    fn enqueue(&self, bytes: &[u8], wait: Option<Duration>) -> Result<u64, SerialError> {
        let deadline = wait.map(|timeout| Instant::now() + timeout);
        let mut state = lock(&self.send);
        loop {
            if self.failed.load(Ordering::SeqCst) {
                return Err(
                    SerialError::new(SerialCause::Disconnected).with_detail("the device is gone")
                );
            }
            if self.closing.load(Ordering::SeqCst) {
                return Err(SerialError::closed());
            }
            let room = state.frames.len() < self.limits.queued_frames
                && state.bytes + bytes.len() <= self.limits.queued_bytes;
            if room {
                let id = state.next_id;
                state.next_id += 1;
                state.bytes += bytes.len();
                state.frames.push_back(Frame {
                    id,
                    bytes: bytes.to_vec(),
                });
                drop(state);
                self.send_room.notify_all();
                return Ok(id);
            }
            let Some(deadline) = deadline else {
                return Err(SerialError::queue_full(
                    self.limits.queued_frames,
                    self.limits.queued_bytes,
                ));
            };
            let now = Instant::now();
            if now >= deadline {
                return Err(SerialError::queue_full(
                    self.limits.queued_frames,
                    self.limits.queued_bytes,
                ));
            }
            let (guard, _) = self
                .send_room
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
    }

    /// Takes the next frame, or `None` when the session is closing.
    fn take_frame(&self) -> Option<Frame> {
        let mut state = lock(&self.send);
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(frame) = state.frames.pop_front() {
                state.in_flight = frame.bytes.len();
                return Some(frame);
            }
            state = self
                .send_room
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Notes that a frame is finished, so the queue has room again.
    fn release_frame(&self, len: usize) {
        let mut state = lock(&self.send);
        state.bytes = state.bytes.saturating_sub(len);
        state.in_flight = 0;
        drop(state);
        self.send_room.notify_all();
    }

    /// Counts and clears what will never be written.
    fn take_discarded(&self) -> (usize, usize) {
        let mut state = lock(&self.send);
        let frames = state.frames.len();
        let bytes: usize = state.frames.iter().map(|frame| frame.bytes.len()).sum();
        state.frames.clear();
        state.bytes = 0;
        state.in_flight = 0;
        (frames, bytes)
    }
}

fn read_loop(port: &Arc<dyn PortIo>, shared: &Arc<Shared>) {
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        if shared.closing.load(Ordering::SeqCst) {
            break;
        }
        // Asked before the read, so a loss reported while the last read was in
        // progress is stated with the bytes around it rather than a poll too late.
        report_overrun(port, shared);
        match port.read(&mut buffer) {
            Ok(read) if read > 0 => {
                shared.deliver(&buffer[..read]);
                report_overrun(port, shared);
            }
            // A port that reports "nothing yet" without a fault is not spinning on a
            // device: yielding here keeps it from burning a core if a driver does that.
            Ok(_) => thread::sleep(Duration::from_millis(1)),
            Err(PortFault::Interrupted) => {
                if shared.closing.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(PortFault::Failed(error)) => {
                shared.note_failure(error);
                break;
            }
        }
    }
}

/// States a receive loss the driver reported, if it reported one.
///
/// The notice is taken whether or not the session is closing: a loss that happened
/// before a close is still part of what the session saw, and dropping it would make
/// the transcript look complete when it is not.
fn report_overrun(port: &Arc<dyn PortIo>, shared: &Arc<Shared>) {
    if let Some(notice) = port.take_notice() {
        shared.push_event(SerialEvent::ReceiveOverflow { notice });
    }
}

fn write_loop(port: &Arc<dyn PortIo>, shared: &Arc<Shared>) {
    while let Some(frame) = shared.take_frame() {
        let requested = frame.bytes.len();
        let mut sent = 0usize;
        while sent < requested {
            if shared.closing.load(Ordering::SeqCst) {
                break;
            }
            match port.write(&frame.bytes[sent..]) {
                // The driver accepted nothing this time; it will take more shortly.
                Ok(0) => thread::sleep(Duration::from_millis(1)),
                Ok(written) => {
                    sent += written;
                    shared.push_event(SerialEvent::Sent {
                        frame: frame.id,
                        bytes: written,
                    });
                }
                Err(PortFault::Interrupted) => {
                    if shared.closing.load(Ordering::SeqCst) {
                        break;
                    }
                }
                Err(PortFault::Failed(error)) => {
                    shared.note_failure(error);
                    shared.release_frame(requested);
                    return;
                }
            }
        }
        if sent < requested {
            // The rest of this frame is not written later and not written twice.
            shared.push_event(SerialEvent::SendIncomplete {
                frame: frame.id,
                sent,
                requested,
            });
        }
        shared.release_frame(requested);
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, SerialError> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .map_err(|error| {
            SerialError::new(SerialCause::Io)
                .with_detail(format!("the {name} thread could not start: {error}"))
        })
}

/// Locks a mutex that this crate never leaves in an inconsistent state.
///
/// A thread that panicked while holding one of these locks cannot have left a queue half
/// updated - every mutation is a single push or pop - so recovering the data is right,
/// and it keeps a panic in one session from poisoning the next.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::config::SerialConfig;
    use crate::io::PortNotice;
    use crate::session::{CloseOutcome, SerialEvent, SessionLimits};
    use crate::testing::{FakeSpec, fake, session_over, session_over_with};

    fn config() -> SerialConfig {
        SerialConfig::default()
    }

    #[test]
    fn closing_an_idle_writer_never_loses_its_wakeup() {
        for _ in 0..100 {
            let port = fake("idle", FakeSpec::new());
            let mut session = session_over_with(
                port,
                config(),
                SessionLimits {
                    close_timeout: Duration::from_secs(1),
                    ..Default::default()
                },
            );
            assert!(matches!(session.close(), CloseOutcome::Closed { .. }));
        }
    }

    /// A close of a session whose queue really did hold frames says how many it
    /// dropped, and the two numbers are the frames that were queued and their bytes.
    ///
    /// The bound this checks is the one R21 and T2 left unasserted: the counts in
    /// [`CloseOutcome::Closed`] are not decoration, so a queue that is discarded on
    /// close is a number the caller can show rather than a silence.
    #[test]
    fn a_close_discards_the_queued_frames_and_counts_them() {
        let port = fake("COM7", FakeSpec::new().holding_writes());
        let mut session = session_over(port.clone(), config());
        let frame = b"abc";
        // The first frame goes to the writer's in-flight slot and stalls inside the
        // driver; the rest stay queued. Waiting for the queue to settle keeps the
        // assertion on which frames were still waiting out of a race with the thread.
        for _ in 0..3 {
            session
                .send(frame)
                .expect("the queue takes three small frames");
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.pending_frames() > 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let queued_frames = session.pending_frames();
        let queued_bytes = session.queued_bytes();
        assert_eq!(
            queued_frames, 2,
            "one frame is in flight and two are still queued"
        );
        assert_eq!(
            queued_bytes,
            frame.len() * 3,
            "the byte count covers the frame being written as well as the queue"
        );

        let outcome = session.close();
        assert_eq!(
            outcome,
            CloseOutcome::Closed {
                discarded_frames: 2,
                discarded_bytes: frame.len() * 2,
            },
            "the counts are the frames and bytes that were never written"
        );
        assert!(port.is_released(), "the port is released after the close");

        let events = session.poll();
        assert!(
            events.iter().any(|event| matches!(
                event,
                SerialEvent::SendIncomplete { sent: 0, requested, .. } if *requested == frame.len()
            )),
            "the frame the driver would not take is reported part way, not replayed: {events:?}"
        );
        assert!(
            events.contains(&SerialEvent::Closed),
            "the release is stated: {events:?}"
        );
    }

    /// A receive loss the driver reports becomes an event of its own.
    ///
    /// A platform that cannot report one never produces this event, which is the
    /// difference between "nothing was lost" and "nobody said" - the second is what
    /// the Unix backend can honestly answer, and it is not turned into the first.
    #[test]
    fn a_driver_overrun_is_reported_as_its_own_event() {
        let notice = PortNotice::overrun(Some(0x0001), None, "the receive buffer overran");
        let port = fake("COM8", FakeSpec::new().with_notice(notice.clone()));
        let mut session = session_over_with(
            port,
            config(),
            SessionLimits {
                close_timeout: Duration::from_millis(200),
                ..SessionLimits::default()
            },
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut seen = None;
        while Instant::now() < deadline {
            let events = session.poll();
            if let Some(found) = events
                .into_iter()
                .find(|event| matches!(event, SerialEvent::ReceiveOverflow { .. }))
            {
                seen = Some(found);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            seen,
            Some(SerialEvent::ReceiveOverflow { notice }),
            "the loss the driver reported reaches the session exactly once"
        );
        assert!(
            !session
                .poll()
                .iter()
                .any(|event| matches!(event, SerialEvent::ReceiveOverflow { .. })),
            "a notice is reported once, not on every poll"
        );
        let _ = session.close();
    }
}
