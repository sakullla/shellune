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
//!    `Drop` cancels, detaches, waits for the worker to exit, then frees: the
//!    callback is gone before the protocol context is torn down, so a late state
//!    event during shutdown is dropped instead of racing the release.
//! 3. A frame is copied out of the library's buffer during the callback. The buffer
//!    belongs to the protocol library and is never retained.
//!
//! When the native libraries are not built, everything here still compiles and
//! links: [`backend_info`] reports the reason and [`DesktopClient::new`] fails
//! with [`Error::Unavailable`] instead of pretending to connect.

mod ffi;
mod mailbox;

use mailbox::Mailbox;
use std::ffi::{CStr, CString, c_void};
use std::fmt;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use ffi::SD_ABI_VERSION;

/// Largest frame this crate accepts from the shim, matching the C header.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Largest clipboard text, in UTF-8 bytes, either direction.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = ffi::SD_MAX_CLIPBOARD_BYTES;
/// Largest clipboard image entry, either direction.
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = ffi::SD_MAX_CLIPBOARD_IMAGE_BYTES;

/// The drive name a mapped local folder gets on the RDP server.
pub const DRIVE_NAME: &str = "Shellune";

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

/// What one clipboard entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipboardKind {
    /// UTF-8 text.
    Text,
    /// A complete PNG file.
    Png,
    /// A packed DIB: a BITMAPINFOHEADER (or a V5 header) followed by the bits, as
    /// Windows' CF_DIB / CF_DIBV5 carry it.
    Dib,
}

impl ClipboardKind {
    fn raw(self) -> u32 {
        match self {
            ClipboardKind::Text => ffi::SD_CLIPBOARD_TEXT_UTF8,
            ClipboardKind::Png => ffi::SD_CLIPBOARD_IMAGE_PNG,
            ClipboardKind::Dib => ffi::SD_CLIPBOARD_IMAGE_DIB,
        }
    }

    fn from_raw(raw: u32) -> Option<ClipboardKind> {
        match raw {
            ffi::SD_CLIPBOARD_TEXT_UTF8 => Some(ClipboardKind::Text),
            ffi::SD_CLIPBOARD_IMAGE_PNG => Some(ClipboardKind::Png),
            ffi::SD_CLIPBOARD_IMAGE_DIB => Some(ClipboardKind::Dib),
            _ => None,
        }
    }

    /// The largest entry of this kind the shim accepts or delivers.
    pub fn bound(self) -> usize {
        match self {
            ClipboardKind::Text => MAX_CLIPBOARD_TEXT_BYTES,
            ClipboardKind::Png | ClipboardKind::Dib => MAX_CLIPBOARD_IMAGE_BYTES,
        }
    }
}

/// One format of one clipboard copy. A group holds each kind at most once.
#[derive(Clone, PartialEq, Eq)]
pub struct ClipboardEntry {
    pub kind: ClipboardKind,
    pub bytes: Vec<u8>,
}

impl ClipboardEntry {
    pub fn text(text: &str) -> ClipboardEntry {
        ClipboardEntry {
            kind: ClipboardKind::Text,
            bytes: text.as_bytes().to_vec(),
        }
    }

    pub fn png(bytes: Vec<u8>) -> ClipboardEntry {
        ClipboardEntry {
            kind: ClipboardKind::Png,
            bytes,
        }
    }

    pub fn dib(bytes: Vec<u8>) -> ClipboardEntry {
        ClipboardEntry {
            kind: ClipboardKind::Dib,
            bytes,
        }
    }

    /// The text of a text entry. Entries from a peer are already valid UTF-8.
    pub fn as_text(&self) -> Option<&str> {
        match self.kind {
            ClipboardKind::Text => std::str::from_utf8(&self.bytes).ok(),
            _ => None,
        }
    }
}

impl fmt::Debug for ClipboardEntry {
    // Images are megabytes; the bytes themselves are never useful in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_text() {
            Some(text) if text.len() <= 64 => f
                .debug_struct("ClipboardEntry")
                .field("kind", &self.kind)
                .field("text", &text)
                .finish(),
            _ => f
                .debug_struct("ClipboardEntry")
                .field("kind", &self.kind)
                .field("len", &self.bytes.len())
                .finish(),
        }
    }
}

/// Something the user should know that is not a state change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Notice {
    /// The server did not open, or refused, the clipboard channel.
    ClipboardRefused,
    /// The VNC server has no extended clipboard and the text had characters outside
    /// Latin-1, so it was not sent.
    ClipboardLatin1Only,
    /// A remote clipboard entry exceeded its bound and was skipped.
    ClipboardTooLarge,
    /// The mapped folder is missing or unreadable; the session connected without it.
    DriveUnavailable,
    Unknown(u32),
}

impl Notice {
    fn from_raw(raw: u32) -> Notice {
        match raw {
            ffi::SD_NOTICE_CLIPBOARD_REFUSED => Notice::ClipboardRefused,
            ffi::SD_NOTICE_CLIPBOARD_LATIN1_ONLY => Notice::ClipboardLatin1Only,
            ffi::SD_NOTICE_CLIPBOARD_TOO_LARGE => Notice::ClipboardTooLarge,
            ffi::SD_NOTICE_DRIVE_UNAVAILABLE => Notice::DriveUnavailable,
            other => Notice::Unknown(other),
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
    /// The peer copied something: every format of that one copy that could be
    /// fetched, each kind at most once. Text bytes that were not UTF-8 are replaced,
    /// so the text may differ from what the peer sent.
    Clipboard { entries: Vec<ClipboardEntry> },
    /// Worth telling the user; the session state is unchanged.
    Notice { notice: Notice },
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

/// Where a translated event goes. A mailbox is the normal shape; a closure lets a
/// caller consume events without a queue, and is also how the native tests prove a
/// panicking consumer cannot unwind into the protocol library.
enum Sink {
    Channel(Arc<Mailbox>),
    Handler(Mutex<Box<dyn FnMut(Event) + Send>>),
}

/// Owns the sink the shim's callback pushes into.
struct Inbox {
    sink: Sink,
}

impl Inbox {
    fn deliver(&self, event: Event) {
        match &self.sink {
            Sink::Channel(sender) => {
                // Compositing is bounded and never waits for the UI: the shim
                // holds its dispatch lock while this callback runs.
                sender.deliver(event);
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
    receiver: Option<Arc<Mailbox>>,
    state: State,
}

// The handle is only touched through the shim, which serialises dispatch and state
// under its own lock; the channel and its receiver are Send.
unsafe impl Send for DesktopClient {}

impl DesktopClient {
    /// A session with a bounded mailbox. Unconsumed frames are composited into
    /// the latest dirty region; intermediate video frames are coalesced. Read with
    /// [`DesktopClient::recv_timeout`] or [`DesktopClient::track_state`].
    pub fn new(protocol: Protocol) -> Result<DesktopClient, Error> {
        let receiver = Arc::new(Mailbox::default());
        let sender = Arc::clone(&receiver);
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
        receiver: Option<Arc<Mailbox>>,
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

    /// Selects the pixel depth VNC frames are delivered in. Only four bytes per
    /// pixel is accepted: the shim pins the RFB pixel format to 32bpp, so it can
    /// never emit a three-byte frame. Any other value returns
    /// [`Error::Argument`] instead of configuring a depth whose frames would all be
    /// dropped.
    pub fn set_vnc_colour_depth(&mut self, bytes_per_pixel: u32) -> Result<(), Error> {
        status(unsafe { ffi::sd_client_set_vnc_colour_depth(self.handle, bytes_per_pixel) })
    }

    /// Clipboard sharing for the next attempt; on unless turned off. Off means
    /// nothing is received and nothing is sent, and [`DesktopClient::push_clipboard`]
    /// answers [`Error::Unavailable`].
    pub fn set_clipboard(&mut self, enabled: bool) -> Result<(), Error> {
        status(unsafe { ffi::sd_client_set_clipboard(self.handle, i32::from(enabled)) })
    }

    /// A local folder the RDP server sees as the drive `name` (normally
    /// [`DRIVE_NAME`]); `None` maps nothing. A folder that is missing or unreadable
    /// when the attempt starts is left out and reported as
    /// [`Notice::DriveUnavailable`]; the session still connects. VNC ignores it.
    pub fn set_drive(&mut self, name: &str, path: Option<&str>) -> Result<(), Error> {
        let name = CString::new(name).map_err(|_| Error::Argument)?;
        let path = path
            .filter(|path| !path.is_empty())
            .map(CString::new)
            .transpose()
            .map_err(|_| Error::Argument)?;
        status(unsafe {
            ffi::sd_client_set_drive(
                self.handle,
                name.as_ptr(),
                path.as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
            )
        })
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

    /// Offers the local clipboard to the remote side, replacing whatever was offered
    /// before; the newest call wins. Each kind may appear once, text up to
    /// [`MAX_CLIPBOARD_TEXT_BYTES`] and an image up to [`MAX_CLIPBOARD_IMAGE_BYTES`];
    /// anything else is [`Error::Argument`]. RDP offers every entry, VNC only the
    /// text. A connection with sharing off answers [`Error::Unavailable`].
    pub fn push_clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), Error> {
        if entries.len() > ffi::SD_MAX_CLIPBOARD_ENTRIES {
            return Err(Error::Argument);
        }
        let mut raw = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.bytes.len() > entry.kind.bound() {
                return Err(Error::Argument);
            }
            raw.push(ffi::sd_clipboard_entry {
                kind: entry.kind.raw(),
                len: entry.bytes.len() as u32,
                bytes: entry.bytes.as_ptr(),
            });
        }
        let code =
            unsafe { ffi::sd_client_clipboard_set(self.handle, raw.as_ptr(), raw.len() as u32) };
        clipboard_status(code)
    }

    /// Offers clipboard text alone; shorthand for [`DesktopClient::push_clipboard`]
    /// with one text entry.
    pub fn clipboard(&self, text: &str) -> Result<(), Error> {
        self.push_clipboard(&[ClipboardEntry::text(text)])
    }

    /// Describes the FreeRDP settings an attempt with this configuration would use,
    /// as `key=value` words. Diagnostic: it proves the clipboard and drive options
    /// reach FreeRDP without a server. RDP builds only.
    #[doc(hidden)]
    pub fn describe_rdp_settings(&self) -> Result<String, Error> {
        let mut buffer = [0 as c_char; 1024];
        let code =
            unsafe { ffi::sd_probe_rdp_settings(self.handle, buffer.as_mut_ptr(), buffer.len()) };
        status(code)?;
        Ok(unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned())
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
        for event in receiver.drain() {
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
        // 1. stop new protocol work, 2. unregister the callback, 3. wait for the
        // worker and its context to go, 4. release the handle, 5. release the inbox.
        //
        // The detach comes before the wait on purpose: the callback must not be
        // reachable while the worker tears its context down (see the crate docs).
        // It is idempotent, so the shim's own detach during free changes nothing.
        let _ = unsafe { ffi::sd_client_cancel(self.handle) };
        unsafe { ffi::sd_client_detach(self.handle) };
        let stopped =
            unsafe { ffi::sd_client_wait(self.handle, SHUTDOWN_TIMEOUT.as_millis() as u32) }
                == ffi::SD_OK;

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

fn clipboard_status(code: i32) -> Result<(), Error> {
    match code {
        ffi::SD_ERR_UNAVAILABLE => Err(Error::Unavailable(
            "clipboard sharing is turned off for this connection".to_string(),
        )),
        other => status(other),
    }
}

/// Copies one clipboard group out of the shim. Entries over their bound, of an
/// unknown kind or repeated are dropped here as well as in C.
fn clipboard_entries(event: &ffi::sd_event) -> Option<Vec<ClipboardEntry>> {
    if event.entries.is_null() || event.entry_count == 0 {
        return None;
    }
    let count = (event.entry_count as usize).min(ffi::SD_MAX_CLIPBOARD_ENTRIES);
    let raw = unsafe { std::slice::from_raw_parts(event.entries, count) };
    let mut entries: Vec<ClipboardEntry> = Vec::with_capacity(count);
    for entry in raw {
        let Some(kind) = ClipboardKind::from_raw(entry.kind) else {
            continue;
        };
        let len = entry.len as usize;
        if len > kind.bound()
            || (len > 0 && entry.bytes.is_null())
            || entries.iter().any(|seen| seen.kind == kind)
        {
            continue;
        }
        let bytes = if len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(entry.bytes, len) }.to_vec()
        };
        let bytes = match kind {
            ClipboardKind::Text => match String::from_utf8(bytes) {
                Ok(text) => text.into_bytes(),
                Err(error) => String::from_utf8_lossy(error.as_bytes())
                    .into_owned()
                    .into_bytes(),
            },
            _ => bytes,
        };
        entries.push(ClipboardEntry { kind, bytes });
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries)
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
        ffi::SD_EVENT_CLIPBOARD => {
            clipboard_entries(event).map(|entries| Event::Clipboard { entries })
        }
        ffi::SD_EVENT_NOTICE => Some(Event::Notice {
            notice: Notice::from_raw(event.notice),
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
///
/// The row width is four bytes per pixel and that is not an assumption: the shim
/// pins its pixel format to 32bpp (sd_client_set_vnc_colour_depth refuses anything
/// else), so a frame with another depth cannot be produced. A stride shorter than
/// the row is still rejected rather than read.
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
    // Copy only the dirty pixels, not the untouched desktop columns between rows.
    let source = unsafe { std::slice::from_raw_parts(event.pixels, bytes) };
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&source[y * stride..y * stride + row]);
    }
    Some(Event::Frame {
        x: event.x,
        y: event.y,
        width,
        height,
        desktop_width: event.width,
        desktop_height: event.height,
        stride: row as u32,
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
/// Returns (size of sd_event, offset of text, offset of host, offset of pixels,
/// offset of entries, offset of notice, size of sd_clipboard_entry).
#[doc(hidden)]
pub fn abi_layout() -> [usize; 7] {
    [
        std::mem::size_of::<ffi::sd_event>(),
        std::mem::offset_of!(ffi::sd_event, text),
        std::mem::offset_of!(ffi::sd_event, host),
        std::mem::offset_of!(ffi::sd_event, pixels),
        std::mem::offset_of!(ffi::sd_event, entries),
        std::mem::offset_of!(ffi::sd_event, notice),
        std::mem::size_of::<ffi::sd_clipboard_entry>(),
    ]
}

/// Runs single pieces of the shim's clipboard handling without a session or a
/// server, through the probes in the C header. These are the functions the
/// backends call, not copies of them; they exist so tests can prove the RDP format
/// choice, the fetch sequence and the text encodings on a machine with no server.
#[doc(hidden)]
pub mod probe {
    use super::{ClipboardKind, Event, ffi, translate};
    use std::ffi::{CString, c_void};
    use std::os::raw::c_char;

    /// One server-to-client message of a scripted RDP clipboard exchange.
    pub enum Step<'a> {
        /// A format list: (format id, registered name).
        FormatList(&'a [(u32, Option<&'a str>)]),
        /// The answer to the request in flight; `None` is CB_RESPONSE_FAIL.
        Response(Option<&'a [u8]>),
    }

    struct Names {
        _owned: Vec<Option<CString>>,
        pointers: Vec<*const c_char>,
        ids: Vec<u32>,
    }

    fn names(formats: &[(u32, Option<&str>)]) -> Names {
        let owned: Vec<Option<CString>> = formats
            .iter()
            .map(|(_, name)| name.and_then(|name| CString::new(name).ok()))
            .collect();
        let pointers = owned
            .iter()
            .map(|name| name.as_ref().map_or(std::ptr::null(), |name| name.as_ptr()))
            .collect();
        Names {
            _owned: owned,
            pointers,
            ids: formats.iter().map(|(id, _)| *id).collect(),
        }
    }

    /// The formats an RDP format list is fetched as, in request order.
    pub fn rdp_select(formats: &[(u32, Option<&str>)]) -> Vec<(u32, ClipboardKind)> {
        let list = names(formats);
        let mut ids = [0u32; 2];
        let mut kinds = [0u32; 2];
        let mut count = 0u32;
        let code = unsafe {
            ffi::sd_probe_rdp_select(
                list.ids.as_ptr(),
                list.pointers.as_ptr(),
                list.ids.len() as u32,
                ids.as_mut_ptr(),
                kinds.as_mut_ptr(),
                &mut count,
            )
        };
        assert_eq!(code, ffi::SD_OK, "sd_probe_rdp_select");
        (0..count.min(2) as usize)
            .filter_map(|i| ClipboardKind::from_raw(kinds[i]).map(|kind| (ids[i], kind)))
            .collect()
    }

    extern "C" fn collect(user: *mut c_void, event: *const ffi::sd_event) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if user.is_null() || event.is_null() {
                return;
            }
            let events = unsafe { &mut *(user as *mut Vec<Event>) };
            if let Some(event) = unsafe { translate(&*event) } {
                events.push(event);
            }
        }));
    }

    /// Plays the steps through the RDP fetch sequence. Returns every format id the
    /// client would request, in order, and the events a session would deliver.
    pub fn rdp_fetch(steps: &[Step<'_>]) -> (Vec<u32>, Vec<Event>) {
        let lists: Vec<Option<Names>> = steps
            .iter()
            .map(|step| match step {
                Step::FormatList(formats) => Some(names(formats)),
                Step::Response(_) => None,
            })
            .collect();
        let raw: Vec<ffi::sd_probe_step> = steps
            .iter()
            .zip(&lists)
            .map(|(step, list)| match (step, list) {
                (Step::FormatList(_), Some(list)) => ffi::sd_probe_step {
                    kind: ffi::SD_PROBE_STEP_FORMAT_LIST,
                    count: list.ids.len() as u32,
                    ids: list.ids.as_ptr(),
                    names: list.pointers.as_ptr(),
                    ok: 0,
                    len: 0,
                    data: std::ptr::null(),
                },
                (Step::Response(answer), _) => ffi::sd_probe_step {
                    kind: ffi::SD_PROBE_STEP_RESPONSE,
                    count: 0,
                    ids: std::ptr::null(),
                    names: std::ptr::null(),
                    ok: i32::from(answer.is_some()),
                    len: answer.map_or(0, |data| data.len() as u32),
                    data: answer.map_or(std::ptr::null(), |data| data.as_ptr()),
                },
                (Step::FormatList(_), None) => unreachable!("every list has its names"),
            })
            .collect();
        let mut requested = vec![0u32; steps.len() * 2 + 2];
        let mut count = 0u32;
        let mut events: Vec<Event> = Vec::new();
        let code = unsafe {
            ffi::sd_probe_rdp_fetch(
                raw.as_ptr(),
                raw.len() as u32,
                requested.as_mut_ptr(),
                requested.len() as u32,
                &mut count,
                collect,
                &mut events as *mut Vec<Event> as *mut c_void,
            )
        };
        assert_eq!(code, ffi::SD_OK, "sd_probe_rdp_fetch");
        requested.truncate((count as usize).min(requested.len()));
        (requested, events)
    }

    fn convert(conversion: i32, input: &[u8]) -> Option<Vec<u8>> {
        let mut needed = 0usize;
        let code = unsafe {
            ffi::sd_probe_text(
                conversion,
                input.as_ptr(),
                input.len(),
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if code != ffi::SD_OK {
            return None;
        }
        let mut out = vec![0u8; needed];
        let mut written = 0usize;
        let code = unsafe {
            ffi::sd_probe_text(
                conversion,
                input.as_ptr(),
                input.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut written,
            )
        };
        (code == ffi::SD_OK && written == needed).then_some(out)
    }

    /// UTF-8 as RDP's CF_UNICODETEXT: UTF-16LE, CRLF line ends, NUL terminated.
    pub fn utf8_to_cf_unicode(text: &str) -> Vec<u8> {
        convert(ffi::SD_PROBE_UTF8_TO_CF_UNICODE, text.as_bytes()).unwrap_or_default()
    }

    /// CF_UNICODETEXT bytes back to UTF-8, up to the first NUL.
    pub fn cf_unicode_to_utf8(bytes: &[u8]) -> Option<Vec<u8>> {
        convert(ffi::SD_PROBE_CF_UNICODE_TO_UTF8, bytes)
    }

    /// UTF-8 as RFB Latin-1, or `None` when a character does not fit.
    pub fn utf8_to_latin1(text: &str) -> Option<Vec<u8>> {
        convert(ffi::SD_PROBE_UTF8_TO_LATIN1, text.as_bytes())
    }

    /// RFB Latin-1 as UTF-8.
    pub fn latin1_to_utf8(bytes: &[u8]) -> Option<Vec<u8>> {
        convert(ffi::SD_PROBE_LATIN1_TO_UTF8, bytes)
    }
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

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn narrow_dirty_regions_do_not_copy_desktop_stride_padding() {
        let mut source = [99u8; 64];
        for row in 0..4 {
            source[row * 16..row * 16 + 4].fill(row as u8);
        }
        // Zero is valid for every integer and pointer field in this ABI record.
        let mut raw: ffi::sd_event = unsafe { std::mem::zeroed() };
        raw.w = 1;
        raw.h = 4;
        raw.width = 4;
        raw.height = 4;
        raw.stride = 16;
        raw.pixels = source.as_ptr();
        let Event::Frame { stride, pixels, .. } = frame(&raw).unwrap() else {
            panic!("frame");
        };
        assert_eq!(stride, 4);
        assert_eq!(
            pixels,
            [vec![0; 4], vec![1; 4], vec![2; 4], vec![3; 4]].concat()
        );
    }
}
