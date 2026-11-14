//! RDP and VNC sessions behind one thin native shim.
//!
//! The protocol libraries (FreeRDP 3.x, LibVNCClient) are compiled from the pinned
//! sources under `vendor/` by `packaging/native/build_native.py`; this crate
//! never sees their headers or structs, only the opaque handle, a flat event struct
//! and a handful of integer calls declared in `ffi.rs`.
//!
//! Three rules hold at this boundary and the tests exercise them:
//!
//! 1. A panic never crosses the C ABI. The callback trampoline catches it, counts
//!    it and drops the event; the protocol worker keeps running.
//! 2. Callbacks are unregistered before anything they captured is released.
//!    `Drop` cancels, waits for the worker to exit, detaches, then frees.
//! 3. A frame is copied out of the library's buffer during the callback. The buffer
//!    belongs to the protocol library and is never retained.
//!
//! When the native libraries are not built, everything here still compiles and
//! links: [`backend_info`] reports the reason and [`DesktopClient::new`] fails
//! with [`Error::Unavailable`] instead of pretending to connect.

mod ffi;

use std::ffi::{CStr, CString, c_void};
use std::fmt;
use std::os::raw::c_char;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

pub use ffi::SD_ABI_VERSION;

/// Largest frame this crate accepts from the shim, matching the C header.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// How long `Drop` waits for a worker to exit before giving up.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(15_000);

/// Incremented whenever a callback panics. A failing callback is a bug in the
/// consumer, but it is contained here instead of aborting the process.
static CALLBACK_PANICS: AtomicU64 = AtomicU64::new(0);
/// Incremented when a handle could not be released because its worker outlived the
/// shutdown timeout. The allocation is leaked on purpose in that case.
static LEAKED_HANDLES: AtomicU64 = AtomicU64::new(0);

pub fn callback_panics() -> u64 {
    CALLBACK_PANICS.load(Ordering::Relaxed)
}

pub fn leaked_handles() -> u64 {
    LEAKED_HANDLES.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Rdp,
    Vnc,
}

impl Protocol {
    fn raw(self) -> i32 {
        match self {
            Protocol::Rdp => ffi::SD_PROTOCOL_RDP,
            Protocol::Vnc => ffi::SD_PROTOCOL_VNC,
        }
    }
}

/// VNC security types a session may negotiate. Types outside the mask are refused
/// before they are attempted, so a server offering only an unbuilt type fails
/// instead of downgrading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VncSecurity(pub u32);

impl VncSecurity {
    pub const NONE: VncSecurity = VncSecurity(ffi::SD_VNC_SECURITY_NONE);
    pub const VNC_AUTH: VncSecurity = VncSecurity(ffi::SD_VNC_SECURITY_VNC_AUTH);
    pub const TLS: VncSecurity = VncSecurity(ffi::SD_VNC_SECURITY_TLS);
    pub const VENCRYPT: VncSecurity = VncSecurity(ffi::SD_VNC_SECURITY_VENCRYPT);

    /// What an unconfigured session asks for: the two types that need no TLS
    /// backend. A UI must show this as unencrypted.
    pub const DEFAULT: VncSecurity = VncSecurity(Self::NONE.0 | Self::VNC_AUTH.0);

    pub fn contains(self, other: VncSecurity) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn with(self, other: VncSecurity) -> VncSecurity {
        VncSecurity(self.0 | other.0)
    }
}

/// Mouse buttons and wheel steps, protocol independent. The shim maps them to
/// PTR_FLAGS_* or the RFB button mask, so the UI never builds a protocol value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerButtons(pub u32);

impl PointerButtons {
    pub const LEFT: PointerButtons = PointerButtons(ffi::SD_BUTTON_LEFT);
    pub const MIDDLE: PointerButtons = PointerButtons(ffi::SD_BUTTON_MIDDLE);
    pub const RIGHT: PointerButtons = PointerButtons(ffi::SD_BUTTON_RIGHT);
    pub const WHEEL_UP: PointerButtons = PointerButtons(ffi::SD_BUTTON_WHEEL_UP);
    pub const WHEEL_DOWN: PointerButtons = PointerButtons(ffi::SD_BUTTON_WHEEL_DOWN);
    pub const WHEEL_LEFT: PointerButtons = PointerButtons(ffi::SD_BUTTON_WHEEL_LEFT);
    pub const WHEEL_RIGHT: PointerButtons = PointerButtons(ffi::SD_BUTTON_WHEEL_RIGHT);

    pub fn contains(self, other: PointerButtons) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn with(self, other: PointerButtons) -> PointerButtons {
        PointerButtons(self.0 | other.0)
    }

    pub fn without(self, other: PointerButtons) -> PointerButtons {
        PointerButtons(self.0 & !other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    None,
    Connect,
    Tls,
    Authentication,
    CertificateRejected,
    UnsupportedSecurity,
    ProtocolFailure,
    Io,
    Cancelled,
    RemoteClosed,
    Limit,
    Internal,
    Unknown(i32),
}

impl Failure {
    fn from_raw(value: u32) -> Failure {
        match value {
            0 => Failure::None,
            1 => Failure::Connect,
            2 => Failure::Tls,
            3 => Failure::Authentication,
            4 => Failure::CertificateRejected,
            5 => Failure::UnsupportedSecurity,
            6 => Failure::ProtocolFailure,
            7 => Failure::Io,
            8 => Failure::Cancelled,
            9 => Failure::RemoteClosed,
            10 => Failure::Limit,
            11 => Failure::Internal,
            other => Failure::Unknown(other as i32),
        }
    }

    fn raw(self) -> i32 {
        match self {
            Failure::None => 0,
            Failure::Connect => 1,
            Failure::Tls => 2,
            Failure::Authentication => 3,
            Failure::CertificateRejected => 4,
            Failure::UnsupportedSecurity => 5,
            Failure::ProtocolFailure => 6,
            Failure::Io => 7,
            Failure::Cancelled => 8,
            Failure::RemoteClosed => 9,
            Failure::Limit => 10,
            Failure::Internal => 11,
            Failure::Unknown(raw) => raw,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Connecting,
    Connected,
    Closing,
    Closed,
    Failed(Failure),
}

impl State {
    fn from_raw(state: u32, failure: u32) -> State {
        match state {
            ffi::SD_STATE_IDLE => State::Idle,
            ffi::SD_STATE_CONNECTING => State::Connecting,
            ffi::SD_STATE_CONNECTED => State::Connected,
            ffi::SD_STATE_CLOSING => State::Closing,
            ffi::SD_STATE_CLOSED => State::Closed,
            ffi::SD_STATE_FAILED => State::Failed(Failure::from_raw(failure)),
            _ => State::Failed(Failure::Unknown(-1)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The session's main state changed. `reason` is the shim's own text: it is
    /// diagnostic material, not a user-facing sentence chosen by the UI.
    State {
        state: State,
        failure: Failure,
        detail: i32,
        reason: String,
    },
    /// A dirty region of the desktop, already copied out of the library's buffer.
    Frame {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        desktop_width: u32,
        desktop_height: u32,
        stride: u32,
        pixels: Vec<u8>,
    },
    /// The server wants a decision about its certificate. Answer with
    /// [`DesktopClient::answer_certificate`]; the request id must be echoed.
    Certificate {
        request_id: u32,
        host: String,
        subject: String,
        issuer: String,
        fingerprint: String,
        flags: u32,
    },
    /// The peer put text on its clipboard. Bytes that are not UTF-8 are replaced,
    /// so the text may differ from what the peer sent.
    Clipboard { text: String },
}

#[derive(Debug)]
pub enum Error {
    /// The handle could not be created (bad arguments or an allocation failure).
    Argument,
    /// This build has no native backend; the string is the reported reason.
    Unavailable(String),
    /// The call is not valid for the client's current state.
    State,
    /// The worker did not finish in time; its resources are not released.
    Timeout,
    Aborted,
    Internal,
    /// The shim and this crate disagree about the ABI.
    Abi(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Argument => write!(f, "invalid argument"),
            Error::Unavailable(reason) => write!(f, "native desktop backend unavailable: {reason}"),
            Error::State => write!(f, "not valid in the current state"),
            Error::Timeout => write!(f, "the session worker did not stop in time"),
            Error::Aborted => write!(f, "aborted"),
            Error::Internal => write!(f, "internal shim failure"),
            Error::Abi(version) => write!(f, "shim ABI {version} does not match {SD_ABI_VERSION}"),
        }
    }
}

impl std::error::Error for Error {}

fn status(code: i32) -> Result<(), Error> {
    match code {
        ffi::SD_OK => Ok(()),
        ffi::SD_ERR_ARGUMENT => Err(Error::Argument),
        ffi::SD_ERR_STATE => Err(Error::State),
        ffi::SD_ERR_ABORTED => Err(Error::Aborted),
        ffi::SD_ERR_UNAVAILABLE => Err(Error::Unavailable("reported by the shim".to_string())),
        ffi::SD_ERR_INTERNAL => Err(Error::Internal),
        _ => Err(Error::Internal),
    }
}

/// What this build can do for a protocol: the linked library versions and the
/// security types that were actually compiled in, or why nothing is linked.
pub fn backend_info(protocol: Protocol) -> Result<String, Error> {
    check_abi()?;
    let mut buffer = [0 as c_char; 512];
    let code = unsafe { ffi::sd_backend_info(protocol.raw(), buffer.as_mut_ptr(), buffer.len()) };
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    match code {
        ffi::SD_OK => Ok(text),
        ffi::SD_ERR_UNAVAILABLE => Err(Error::Unavailable(text)),
        ffi::SD_ERR_ARGUMENT => Err(Error::Argument),
        _ => Err(Error::Internal),
    }
}

fn check_abi() -> Result<(), Error> {
    let version = unsafe { ffi::sd_abi_version() };
    if version == SD_ABI_VERSION {
        Ok(())
    } else {
        Err(Error::Abi(version))
    }
}

/// Where a translated event goes. A channel is the normal shape; a closure lets a
/// caller consume events without a queue, and is also how the native tests prove a
/// panicking consumer cannot unwind into the protocol library.
enum Sink {
    Channel(Sender<Event>),
    Handler(Mutex<Box<dyn FnMut(Event) + Send>>),
}

/// Owns the sink the shim's callback pushes into. Dropping it closes the channel,
/// which is how a consumer learns that no further event can arrive.
struct Inbox {
    sink: Sink,
}

impl Inbox {
    fn deliver(&self, event: Event) {
        match &self.sink {
            Sink::Channel(sender) => {
                // A closed receiver means the consumer is gone; the protocol work
                // goes on, so nothing is retried here.
                let _ = sender.send(event);
            }
            Sink::Handler(handler) => {
                // A poisoned lock means the previous call panicked. The trampoline
                // already contained it, so recover the handler and keep delivering.
                let mut handler = match handler.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                handler(event);
            }
        }
    }
}

/// A desktop session handle.
///
/// The lifetime rules are enforced here, not by the caller: the worker is always
/// stopped before the context is freed, and the callback is unregistered before the
/// channel it feeds is dropped.
pub struct DesktopClient {
    protocol: Protocol,
    handle: *mut ffi::sd_client,
    inbox: *mut Inbox,
    receiver: Option<Receiver<Event>>,
    state: State,
}

// The handle is only touched through the shim, which serialises dispatch and state
// under its own lock; the channel and its receiver are Send.
unsafe impl Send for DesktopClient {}

impl DesktopClient {
    /// A session whose events arrive on a channel; read them with
    /// [`DesktopClient::recv_timeout`] or [`DesktopClient::track_state`].
    pub fn new(protocol: Protocol) -> Result<DesktopClient, Error> {
        let (sender, receiver) = mpsc::channel();
        DesktopClient::create(protocol, Sink::Channel(sender), Some(receiver))
    }

    /// A session that hands every event to `handler` on the protocol's own worker
    /// thread. A panic in the handler is caught at the ABI boundary: the session
    /// keeps running and [`callback_panics`] counts it.
    pub fn with_handler<F>(protocol: Protocol, handler: F) -> Result<DesktopClient, Error>
    where
        F: FnMut(Event) + Send + 'static,
    {
        DesktopClient::create(protocol, Sink::Handler(Mutex::new(Box::new(handler))), None)
    }

    /// Builds the handle once, so a value that owns a live shim handle is never
    /// copied or partially moved into another one: dropping the copy would free the
    /// handle the new value still points at.
    fn create(
        protocol: Protocol,
        sink: Sink,
        receiver: Option<Receiver<Event>>,
    ) -> Result<DesktopClient, Error> {
        check_abi()?;
        backend_info(protocol)?;

        let inbox = Box::new(Inbox { sink });
        let user = Box::into_raw(inbox) as *mut c_void;

        let mut handle: *mut ffi::sd_client = std::ptr::null_mut();
        let code = unsafe { ffi::sd_client_new(protocol.raw(), dispatch, user, &mut handle) };
        if code != ffi::SD_OK || handle.is_null() {
            // The shim only ever stores the pointer, so it is still ours to drop.
            unsafe { drop(Box::from_raw(user as *mut Inbox)) };
            return match code {
                ffi::SD_ERR_ARGUMENT => Err(Error::Argument),
                _ => Err(Error::Internal),
            };
        }
        Ok(DesktopClient {
            protocol,
            handle,
            inbox: user as *mut Inbox,
            receiver,
            state: State::Idle,
        })
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn set_server(&mut self, host: &str, port: u16) -> Result<(), Error> {
        let host = CString::new(host).map_err(|_| Error::Argument)?;
        status(unsafe { ffi::sd_client_set_server(self.handle, host.as_ptr(), port) })
    }

    pub fn set_credentials(
        &mut self,
        user: Option<&str>,
        password: Option<&str>,
        domain: Option<&str>,
    ) -> Result<(), Error> {
        let user = user
            .map(CString::new)
            .transpose()
            .map_err(|_| Error::Argument)?;
        let password = password
            .map(CString::new)
            .transpose()
            .map_err(|_| Error::Argument)?;
        let domain = domain
            .map(CString::new)
            .transpose()
            .map_err(|_| Error::Argument)?;
        status(unsafe {
            ffi::sd_client_set_credentials(
                self.handle,
                user.as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                password
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                domain
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
            )
        })
    }

    pub fn set_size(&mut self, width: u32, height: u32) -> Result<(), Error> {
        status(unsafe { ffi::sd_client_set_size(self.handle, width, height) })
    }

    pub fn set_vnc_security(&mut self, security: VncSecurity) -> Result<(), Error> {
        status(unsafe { ffi::sd_client_set_vnc_security(self.handle, security.0) })
    }

    pub fn set_vnc_colour_depth(&mut self, bytes_per_pixel: u32) -> Result<(), Error> {
        status(unsafe { ffi::sd_client_set_vnc_colour_depth(self.handle, bytes_per_pixel) })
    }

    pub fn connect(&mut self) -> Result<(), Error> {
        let code = unsafe { ffi::sd_client_connect(self.handle) };
        if code == ffi::SD_OK {
            self.state = State::Connecting;
        }
        status(code)
    }

    /// Aborts a connect in progress or closes a live session. Safe from any thread
    /// and safe to call twice.
    pub fn cancel(&mut self) -> Result<(), Error> {
        let code = unsafe { ffi::sd_client_cancel(self.handle) };
        if code == ffi::SD_OK {
            self.state = State::Closing;
        }
        status(code)
    }

    /// Waits for the worker to exit, which is the moment the protocol context is
    /// gone. Returns [`Error::Timeout`] when it outlives `timeout`.
    pub fn wait_for_worker(&self, timeout: Duration) -> Result<(), Error> {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { ffi::sd_client_wait(self.handle, millis) } {
            ffi::SD_OK => Ok(()),
            ffi::SD_ERR_STATE => Err(Error::Timeout),
            ffi::SD_ERR_ARGUMENT => Err(Error::Argument),
            _ => Err(Error::Internal),
        }
    }

    pub fn answer_certificate(&self, request_id: u32, accept: bool) -> Result<(), Error> {
        status(unsafe {
            ffi::sd_client_answer_certificate(self.handle, request_id, i32::from(accept))
        })
    }

    /// Submits a key press or release. `scancode` drives RDP, `keysym` drives
    /// VNC; each backend uses the form its protocol speaks.
    pub fn key(&self, scancode: u32, keysym: u32, down: bool, extended: bool) -> Result<(), Error> {
        status(unsafe {
            ffi::sd_client_key(
                self.handle,
                scancode,
                keysym,
                i32::from(down),
                i32::from(extended),
            )
        })
    }

    pub fn pointer(&self, x: u16, y: u16, buttons: PointerButtons) -> Result<(), Error> {
        status(unsafe { ffi::sd_client_pointer(self.handle, x, y, buttons.0) })
    }

    pub fn clipboard(&self, text: &str) -> Result<(), Error> {
        let text = CString::new(text).map_err(|_| Error::Argument)?;
        status(unsafe { ffi::sd_client_clipboard(self.handle, text.as_ptr()) })
    }

    /// Blocks until an event arrives or `timeout` passes. Only a session created
    /// with [`DesktopClient::new`] has a queue; a handler session is disconnected.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        match &self.receiver {
            Some(receiver) => receiver.recv_timeout(timeout),
            None => Err(RecvTimeoutError::Disconnected),
        }
    }

    /// Returns the next queued event without blocking.
    pub fn try_recv(&self) -> Result<Event, TryRecvError> {
        match &self.receiver {
            Some(receiver) => receiver.try_recv(),
            None => Err(TryRecvError::Disconnected),
        }
    }

    /// Drains the queue and records the latest main state seen in it.
    pub fn track_state(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(receiver) = &self.receiver else {
            return events;
        };
        while let Ok(event) = receiver.try_recv() {
            if let Event::State { state, .. } = &event {
                self.state = *state;
            }
            events.push(event);
        }
        events
    }
}

impl Drop for DesktopClient {
    fn drop(&mut self) {
        // 1. stop new protocol work, 2. wait for the worker and its context to go,
        // 3. unregister the callback, 4. release the handle, 5. release the inbox.
        let _ = unsafe { ffi::sd_client_cancel(self.handle) };
        let stopped =
            unsafe { ffi::sd_client_wait(self.handle, SHUTDOWN_TIMEOUT.as_millis() as u32) }
                == ffi::SD_OK;
        unsafe { ffi::sd_client_detach(self.handle) };

        if !stopped {
            LEAKED_HANDLES.fetch_add(1, Ordering::Relaxed);
            // The worker still owns the context and may still call the inbox: leak
            // both rather than free memory a library thread is writing to.
            if !self.inbox.is_null() {
                unsafe { drop(Box::from_raw(self.inbox)) };
                self.inbox = std::ptr::null_mut();
            }
            return;
        }

        let freed = unsafe { ffi::sd_client_free(self.handle) };
        if freed != ffi::SD_OK {
            LEAKED_HANDLES.fetch_add(1, Ordering::Relaxed);
            eprintln!("shellune-desktop: the shim refused to free a handle (code {freed})");
        }
        if !self.inbox.is_null() {
            unsafe { drop(Box::from_raw(self.inbox)) };
            self.inbox = std::ptr::null_mut();
        }
        if freed == ffi::SD_OK {
            self.handle = std::ptr::null_mut();
        }
    }
}

/// The single trampoline every session shares. It must never unwind: a panic that
/// reached the C worker would unwind into FreeRDP or LibVNCClient frames.
extern "C" fn dispatch(user: *mut c_void, event: *const ffi::sd_event) {
    let outcome = std::panic::catch_unwind(|| {
        if user.is_null() || event.is_null() {
            return;
        }
        let inbox = unsafe { &*(user as *const Inbox) };
        if let Some(event) = unsafe { translate(&*event) } {
            inbox.deliver(event);
        }
    });
    if outcome.is_err() {
        CALLBACK_PANICS.fetch_add(1, Ordering::Relaxed);
    }
}

fn text(ptr: *const c_char, len: u32) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

unsafe fn translate(event: &ffi::sd_event) -> Option<Event> {
    if event.abi_version != SD_ABI_VERSION {
        return None;
    }
    match event.kind {
        ffi::SD_EVENT_STATE => Some(Event::State {
            state: State::from_raw(event.state, event.failure),
            failure: Failure::from_raw(event.failure),
            detail: event.detail,
            reason: text(event.text, event.text_len),
        }),
        ffi::SD_EVENT_CLIPBOARD => Some(Event::Clipboard {
            text: text(event.text, event.text_len),
        }),
        ffi::SD_EVENT_CERTIFICATE => Some(Event::Certificate {
            request_id: event.request_id,
            host: c_string(event.host),
            subject: c_string(event.subject),
            issuer: c_string(event.issuer),
            fingerprint: c_string(event.fingerprint),
            flags: event.cert_flags,
        }),
        ffi::SD_EVENT_FRAME => frame(event),
        _ => None,
    }
}

/// Copies one dirty region. The bounds are checked here as well as in C because a
/// wrong stride would otherwise become an out-of-bounds read in this process.
fn frame(event: &ffi::sd_event) -> Option<Event> {
    let width = event.w;
    let height = event.h;
    if width <= 0 || height <= 0 || event.pixels.is_null() {
        return None;
    }
    let row = (width as usize).checked_mul(4)?;
    let stride = event.stride as usize;
    if stride < row {
        return None;
    }
    let bytes = (height as usize - 1)
        .checked_mul(stride)?
        .checked_add(row)?;
    if bytes > MAX_FRAME_BYTES {
        return None;
    }
    let pixels = unsafe { std::slice::from_raw_parts(event.pixels, bytes) }.to_vec();
    Some(Event::Frame {
        x: event.x,
        y: event.y,
        width,
        height,
        desktop_width: event.width,
        desktop_height: event.height,
        stride: event.stride,
        pixels,
    })
}

/// The shim's own name for a status code, for diagnostics only.
pub fn status_name(code: i32) -> String {
    let _ = check_abi();
    unsafe { CStr::from_ptr(ffi::sd_status_name(code)) }
        .to_string_lossy()
        .into_owned()
}

/// The numbers the C header promises, so the test can prove both sides agree.
/// Returns (size of sd_event, offset of text, offset of host, offset of pixels).
#[doc(hidden)]
pub fn abi_layout() -> [usize; 4] {
    [
        std::mem::size_of::<ffi::sd_event>(),
        std::mem::offset_of!(ffi::sd_event, text),
        std::mem::offset_of!(ffi::sd_event, host),
        std::mem::offset_of!(ffi::sd_event, pixels),
    ]
}

/// The shim's own name for a session state, for diagnostics only.
pub fn state_name(state: State) -> String {
    let raw = match state {
        State::Idle => ffi::SD_STATE_IDLE,
        State::Connecting => ffi::SD_STATE_CONNECTING,
        State::Connected => ffi::SD_STATE_CONNECTED,
        State::Closing => ffi::SD_STATE_CLOSING,
        State::Closed => ffi::SD_STATE_CLOSED,
        State::Failed(_) => ffi::SD_STATE_FAILED,
    };
    unsafe { CStr::from_ptr(ffi::sd_state_name(raw as i32)) }
        .to_string_lossy()
        .into_owned()
}

/// The shim's own name for a failure kind, for diagnostics only.
pub fn failure_name(failure: Failure) -> String {
    unsafe { CStr::from_ptr(ffi::sd_failure_name(failure.raw())) }
        .to_string_lossy()
        .into_owned()
}
