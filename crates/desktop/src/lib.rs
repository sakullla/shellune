//! RDP and VNC sessions behind one API.
//!
//! Both protocols are implemented in this crate: RDP in `rdp` (its own socket,
//! TLS, MCS, share and channel code) and VNC in `vnc` (its own socket, RFB
//! handshake, authenticated security types, pixel updates and clipboard). Neither
//! calls a protocol library.
//!
//! Three rules hold on the way in and out of a session, and the tests exercise
//! them:
//!
//! 1. A panic in a consumer callback never reaches protocol code. The delivery
//!    path catches it, counts it and drops the event; the worker keeps running.
//! 2. A worker is stopped and joined before anything it captured is released, so
//!    no event can arrive after the sink it feeds is gone.
//! 3. A frame is copied out of the engine's buffer before it is delivered; the
//!    buffer is never shared with a consumer.
//!
//! [`backend_info`] reports which engine answers a protocol; a session never
//! claims a capability the binary does not have.

mod control;
mod mailbox;
mod rdp;
mod tls;
mod vnc;

use mailbox::Mailbox;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Largest frame this crate accepts, in bytes.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Largest clipboard text, in UTF-8 bytes, either direction.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
/// Largest clipboard image entry, either direction.
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// One entry per kind at most, so a group never holds more than this.
pub const MAX_CLIPBOARD_ENTRIES: usize = 3;

/// The drive name a mapped local folder gets on the RDP server.
pub const DRIVE_NAME: &str = "Shellune";

// Pointer button bits, protocol independent: the UI names a button and each engine
// maps it to its own form.
pub(crate) const SD_BUTTON_LEFT: u32 = 0x01;
pub(crate) const SD_BUTTON_MIDDLE: u32 = 0x02;
pub(crate) const SD_BUTTON_RIGHT: u32 = 0x04;
pub(crate) const SD_BUTTON_WHEEL_UP: u32 = 0x08;
pub(crate) const SD_BUTTON_WHEEL_DOWN: u32 = 0x10;
pub(crate) const SD_BUTTON_WHEEL_LEFT: u32 = 0x20;
pub(crate) const SD_BUTTON_WHEEL_RIGHT: u32 = 0x40;

// VNC security types a session may negotiate. A bit outside the mask is refused
// before it is attempted, so a server offering only an unsupported type fails
// instead of silently downgrading.
const SD_VNC_SECURITY_NONE: u32 = 0x1;
const SD_VNC_SECURITY_VNC_AUTH: u32 = 0x2;
const SD_VNC_SECURITY_TLS: u32 = 0x4;
const SD_VNC_SECURITY_VENCRYPT: u32 = 0x8;

/// Incremented whenever a consumer callback panics. A failing callback is a bug in
/// the consumer, but it is contained here instead of ending the worker.
static CALLBACK_PANICS: AtomicU64 = AtomicU64::new(0);

pub fn callback_panics() -> u64 {
    CALLBACK_PANICS.load(Ordering::Relaxed)
}

/// Incremented when a session's worker outlived the shutdown wait and was left to
/// finish on its own. Its event sink stays alive with it, so nothing it delivers
/// reaches freed memory; the count is a diagnostic, not a leak of the handle.
static LEAKED_HANDLES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn count_leaked_handle() {
    LEAKED_HANDLES.fetch_add(1, Ordering::Relaxed);
}

pub fn leaked_handles() -> u64 {
    LEAKED_HANDLES.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Rdp,
    Vnc,
}

/// VNC security types a session may negotiate. Types outside the mask are refused
/// before they are attempted, so a server offering only an unaccepted type fails
/// instead of downgrading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VncSecurity(pub u32);

impl VncSecurity {
    pub const NONE: VncSecurity = VncSecurity(SD_VNC_SECURITY_NONE);
    pub const VNC_AUTH: VncSecurity = VncSecurity(SD_VNC_SECURITY_VNC_AUTH);
    /// Anonymous Diffie-Hellman TLS, RFB security type 18. This build has no
    /// anonymous cipher suites, so a server that offers only this type is refused
    /// with Failure::UnsupportedSecurity; the bit is kept so a host can record the
    /// policy it asked for.
    pub const TLS: VncSecurity = VncSecurity(SD_VNC_SECURITY_TLS);
    /// VeNCrypt (RFB security type 19). Its certificate forms (X509Vnc, X509None)
    /// are the encrypted sessions this build completes; its anonymous TLS forms
    /// are refused for the same reason as VncSecurity::TLS.
    pub const VENCRYPT: VncSecurity = VncSecurity(SD_VNC_SECURITY_VENCRYPT);

    /// What an unconfigured session asks for: the two types that need no
    /// certificate. A UI must show this as unencrypted.
    pub const DEFAULT: VncSecurity = VncSecurity(Self::NONE.0 | Self::VNC_AUTH.0);

    pub fn contains(self, other: VncSecurity) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn with(self, other: VncSecurity) -> VncSecurity {
        VncSecurity(self.0 | other.0)
    }
}

/// Mouse buttons and wheel steps, protocol independent. Each engine maps them to
/// its protocol's own form, so the UI never builds a protocol value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerButtons(pub u32);

impl PointerButtons {
    pub const LEFT: PointerButtons = PointerButtons(SD_BUTTON_LEFT);
    pub const MIDDLE: PointerButtons = PointerButtons(SD_BUTTON_MIDDLE);
    pub const RIGHT: PointerButtons = PointerButtons(SD_BUTTON_RIGHT);
    pub const WHEEL_UP: PointerButtons = PointerButtons(SD_BUTTON_WHEEL_UP);
    pub const WHEEL_DOWN: PointerButtons = PointerButtons(SD_BUTTON_WHEEL_DOWN);
    pub const WHEEL_LEFT: PointerButtons = PointerButtons(SD_BUTTON_WHEEL_LEFT);
    pub const WHEEL_RIGHT: PointerButtons = PointerButtons(SD_BUTTON_WHEEL_RIGHT);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Connecting,
    Connected,
    Closing,
    Closed,
    Failed(Failure),
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
    /// The bit that names this kind in a group. One group carries each kind once.
    fn bit(self) -> u32 {
        match self {
            ClipboardKind::Text => 1,
            ClipboardKind::Png => 2,
            ClipboardKind::Dib => 4,
        }
    }

    /// The largest entry of this kind a session accepts or delivers.
    pub fn bound(self) -> usize {
        match self {
            ClipboardKind::Text => MAX_CLIPBOARD_TEXT_BYTES,
            ClipboardKind::Png | ClipboardKind::Dib => MAX_CLIPBOARD_IMAGE_BYTES,
        }
    }
}

/// The length of the longest prefix of the UTF-8 text `bytes` that fits in `bound`
/// bytes and ends on a whole character: `bytes.len()` when it already fits.
pub fn cut_clipboard_text(bytes: &[u8], bound: usize) -> usize {
    if bytes.len() <= bound {
        return bytes.len();
    }
    // The byte at `bound` is the first one left out; backing off over continuation
    // bytes reaches the start of its character, which is then left out whole.
    let mut at = bound;
    while at > 0 && bytes[at] & 0xC0 == 0x80 {
        at -= 1;
    }
    at
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
    /// The server refused the clipboard: it failed this client's format list, or its
    /// clipboard never became ready while a local copy waited. RDP only.
    ClipboardRefused,
    /// The clipboard became ready after [`Notice::ClipboardRefused`] was reported
    /// (the server was still logging on); that notice no longer holds.
    ClipboardReady,
    /// The VNC server has no extended clipboard and the text had characters outside
    /// Latin-1, so it was not sent.
    ClipboardLatin1Only,
    /// A clipboard entry of this kind exceeded its bound. Text is cut at the last
    /// whole character within the bound and still delivered or sent; a remote image
    /// is left out of its group.
    ClipboardTooLarge(ClipboardKind),
    /// The mapped folder could not be offered in this session, for this reason; the
    /// session goes on without it.
    DriveUnavailable(DriveProblem),
    /// The server did not take the mapped folder; the session goes on without it.
    DriveRefused(DriveRefusal),
    Unknown(u32),
}

/// How the server did not take the mapped folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DriveRefusal {
    /// The server never opened device redirection, for example because its policy
    /// turns all of it off.
    NotOpened,
    /// The server declined the drive with this NTSTATUS, for example because its
    /// policy turns drive redirection off.
    Declined(u32),
}

/// Why the mapped folder is not available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DriveProblem {
    /// The folder does not exist.
    Missing,
    /// The path names a file.
    NotAFolder,
    /// Not a drive-letter or UNC path.
    NotAbsolute,
    /// Longer than the drive channel can address (MAX_PATH, counted both in UTF-16
    /// units and in UTF-8 bytes).
    TooLong,
    /// The folder exists but cannot be listed.
    NoAccess,
    /// The drive channel could not set the drive up.
    NotAdded,
    Other(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The session's main state changed. `reason` is the engine's own text: it is
    /// diagnostic material, not a user-facing sentence chosen by the UI.
    State {
        state: State,
        failure: Failure,
        detail: i32,
        reason: String,
    },
    /// A dirty region of the desktop, already copied out of the engine's buffer.
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
    /// The configuration or the session cannot provide what was asked for; the
    /// string is the reported reason.
    Unavailable(String),
    /// The call is not valid for the client's current state.
    State,
    /// The worker did not finish in time; it is still joined when the handle drops.
    Timeout,
    Aborted,
    Internal,
    /// A diagnostics table from another build. The names are part of this crate, so
    /// it cannot disagree with itself; the variant stays because callers match it.
    Abi(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Argument => write!(f, "invalid argument"),
            Error::Unavailable(reason) => write!(f, "unavailable: {reason}"),
            Error::State => write!(f, "not valid in the current state"),
            Error::Timeout => write!(f, "the session worker did not stop in time"),
            Error::Aborted => write!(f, "aborted"),
            Error::Internal => write!(f, "internal failure"),
            Error::Abi(version) => {
                write!(f, "diagnostics table version {version} is not this build")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The protocols this build carries and what each can negotiate. Both engines are
/// this crate, so there is no native library whose version could differ.
pub fn backend_info(protocol: Protocol) -> Result<String, Error> {
    Ok(match protocol {
        Protocol::Rdp => "shellune RDP client (native, no external library)".to_owned(),
        Protocol::Vnc => "shellune VNC client (native, no external library) \
             security=none,vncauth,vencrypt-x509 anonymous-tls=refused \
             encodings=raw,copyrect,hextile clipboard=text"
            .to_owned(),
    })
}

/// Where an event goes. A mailbox is the normal shape; a closure lets a caller
/// consume events without a queue, and is also how the tests prove a panicking
/// consumer cannot end the worker.
enum Sink {
    Channel(Arc<Mailbox>),
    Handler(Mutex<Box<dyn FnMut(Event) + Send>>),
}

/// Sends one event to whichever sink this handle was built with. Both engines use
/// it, so the mailbox and handler rules stay in one place.
fn deliver(sink: &Sink, event: Event) {
    match sink {
        Sink::Channel(sender) => {
            // Compositing is bounded and never waits for the UI.
            sender.deliver(event);
        }
        Sink::Handler(handler) => {
            // The handler belongs to the consumer and may panic. Catching it here
            // keeps that panic out of the protocol worker, which keeps running.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut handler = match handler.lock() {
                    Ok(guard) => guard,
                    // A poisoned lock means the previous call panicked; recover the
                    // handler and keep delivering.
                    Err(poisoned) => poisoned.into_inner(),
                };
                handler(event);
            }));
            if outcome.is_err() {
                CALLBACK_PANICS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// A desktop session handle.
///
/// The lifetime rules are enforced here, not by the caller: the worker is always
/// stopped and joined before anything it feeds is released.
pub struct DesktopClient {
    protocol: Protocol,
    backend: Backend,
    receiver: Option<Arc<Mailbox>>,
    state: State,
}

/// Which engine answers a handle.
enum Backend {
    Rdp(Box<rdp::Client>),
    Vnc(Box<vnc::Client>),
}

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

    /// Wraps the configured sink into the single emitter a session calls.
    fn create(
        protocol: Protocol,
        sink: Sink,
        receiver: Option<Arc<Mailbox>>,
    ) -> Result<DesktopClient, Error> {
        let sink = Arc::new(sink);
        let emit_sink = Arc::clone(&sink);
        let emit: control::Emit = Arc::new(move |event| deliver(&emit_sink, event));
        let backend = match protocol {
            Protocol::Rdp => Backend::Rdp(Box::new(rdp::Client::new(emit))),
            Protocol::Vnc => Backend::Vnc(Box::new(vnc::Client::new(emit))),
        };
        Ok(DesktopClient {
            protocol,
            backend,
            receiver,
            state: State::Idle,
        })
    }

    /// The RDP engine when this handle is one.
    fn rdp(&self) -> Option<&rdp::Client> {
        match &self.backend {
            Backend::Rdp(client) => Some(client),
            Backend::Vnc(_) => None,
        }
    }

    fn rdp_mut(&mut self) -> Option<&mut rdp::Client> {
        match &mut self.backend {
            Backend::Rdp(client) => Some(client),
            Backend::Vnc(_) => None,
        }
    }

    /// The VNC engine when this handle is one.
    fn vnc(&self) -> Option<&vnc::Client> {
        match &self.backend {
            Backend::Rdp(_) => None,
            Backend::Vnc(client) => Some(client),
        }
    }

    fn vnc_mut(&mut self) -> Option<&mut vnc::Client> {
        match &mut self.backend {
            Backend::Rdp(_) => None,
            Backend::Vnc(client) => Some(client),
        }
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn state(&self) -> State {
        match self.backend {
            Backend::Rdp(ref client) => client.state(),
            Backend::Vnc(ref client) => client.state(),
        }
    }

    pub fn set_server(&mut self, host: &str, port: u16) -> Result<(), Error> {
        if let Some(client) = self.rdp_mut() {
            client.set_server(host, port);
            return Ok(());
        }
        if let Some(client) = self.vnc_mut() {
            client.set_server(host, port);
            return Ok(());
        }
        Err(Error::State)
    }

    pub fn set_credentials(
        &mut self,
        user: Option<&str>,
        password: Option<&str>,
        domain: Option<&str>,
    ) -> Result<(), Error> {
        if let Some(client) = self.rdp_mut() {
            client.set_credentials(user, password, domain);
            return Ok(());
        }
        if let Some(client) = self.vnc_mut() {
            // RFB authenticates with one secret; a user name or domain has no
            // place in it.
            client.set_password(password);
            return Ok(());
        }
        Err(Error::State)
    }

    /// The size an attempt may ask for. RDP negotiates it; a VNC server owns its
    /// desktop size, so the value only has to be within the frame bound.
    pub fn set_size(&mut self, width: u32, height: u32) -> Result<(), Error> {
        // A desktop whose frame cannot fit the accepted size is refused here,
        // before an attempt allocates it. Both protocols share the bound.
        if u64::from(width) * u64::from(height) * 4 > MAX_FRAME_BYTES as u64
            || width == 0
            || height == 0
            || width > u32::from(u16::MAX)
            || height > u32::from(u16::MAX)
        {
            return Err(Error::Argument);
        }
        if let Some(client) = self.rdp_mut() {
            client.set_size(width, height);
            return Ok(());
        }
        if let Some(client) = self.vnc_mut() {
            client.set_size(width, height);
            return Ok(());
        }
        Err(Error::State)
    }

    /// The security types a VNC attempt may negotiate. RDP has its own negotiated
    /// stack and answers [`Error::State`].
    pub fn set_vnc_security(&mut self, security: VncSecurity) -> Result<(), Error> {
        match self.vnc_mut() {
            Some(client) => {
                client.set_security(security.0);
                Ok(())
            }
            None => Err(Error::State),
        }
    }

    /// Selects the pixel depth VNC frames are delivered in. Only four bytes per
    /// pixel is accepted: the engine pins its RFB pixel format to 32bpp, so it can
    /// never emit a three-byte frame. Any other value returns
    /// [`Error::Argument`] instead of configuring a depth whose frames would all be
    /// dropped.
    pub fn set_vnc_colour_depth(&mut self, bytes_per_pixel: u32) -> Result<(), Error> {
        match self.vnc() {
            Some(client) => client.set_depth(bytes_per_pixel),
            None => Err(Error::State),
        }
    }

    /// Clipboard sharing for the next attempt; on unless turned off. Off means
    /// nothing is received and nothing is sent, and [`DesktopClient::push_clipboard`]
    /// answers [`Error::Unavailable`].
    pub fn set_clipboard(&mut self, enabled: bool) -> Result<(), Error> {
        if let Some(client) = self.rdp_mut() {
            client.set_clipboard(enabled);
            return Ok(());
        }
        if let Some(client) = self.vnc_mut() {
            client.set_clipboard(enabled);
            return Ok(());
        }
        Err(Error::State)
    }

    /// A local folder the RDP server sees as the drive `name` (normally
    /// [`DRIVE_NAME`]); `None` maps nothing. A folder that cannot be offered when the
    /// attempt starts is reported as [`Notice::DriveUnavailable`] with the reason; one
    /// the server does not take is reported as [`Notice::DriveRefused`] with how it was
    /// refused. Either way the session still connects.
    /// The server stays inside the folder: requests through a symbolic link or
    /// junction in it, or naming a DOS device, are answered with access denied.
    /// VNC ignores it.
    pub fn set_drive(&mut self, name: &str, path: Option<&str>) -> Result<(), Error> {
        if let Some(client) = self.rdp_mut() {
            if name != DRIVE_NAME {
                return Err(Error::Argument);
            }
            let folder = path
                .filter(|path| !path.is_empty())
                .map(std::path::PathBuf::from);
            client.set_drive(folder);
            return Ok(());
        }
        if self.vnc().is_some() {
            // RFB has no channel for a mapped folder; the setting is accepted and
            // has nothing to act on.
            return if name == DRIVE_NAME {
                Ok(())
            } else {
                Err(Error::Argument)
            };
        }
        Err(Error::State)
    }

    pub fn connect(&mut self) -> Result<(), Error> {
        if let Some(client) = self.rdp_mut() {
            client.connect()?;
            self.state = State::Connecting;
            return Ok(());
        }
        if let Some(client) = self.vnc_mut() {
            client.connect()?;
            self.state = State::Connecting;
            return Ok(());
        }
        Err(Error::State)
    }

    /// Aborts a connect in progress or closes a live session. Safe from any thread
    /// and safe to call twice.
    pub fn cancel(&mut self) -> Result<(), Error> {
        if let Some(client) = self.rdp_mut() {
            client.cancel();
            self.state = State::Closing;
            return Ok(());
        }
        if let Some(client) = self.vnc_mut() {
            client.cancel();
            self.state = State::Closing;
            return Ok(());
        }
        Err(Error::State)
    }

    /// Waits for the worker to exit, which is the moment the protocol context is
    /// gone. Returns [`Error::Timeout`] when it outlives `timeout`.
    pub fn wait_for_worker(&mut self, timeout: Duration) -> Result<(), Error> {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        if let Some(client) = self.rdp_mut() {
            return client.wait(millis);
        }
        if let Some(client) = self.vnc_mut() {
            return client.wait(millis);
        }
        Err(Error::State)
    }

    pub fn answer_certificate(&self, request_id: u32, accept: bool) -> Result<(), Error> {
        if let Some(client) = self.rdp() {
            return client.answer_certificate(request_id, accept);
        }
        match self.vnc() {
            Some(client) => client.answer_certificate(request_id, accept),
            None => Err(Error::State),
        }
    }

    /// Submits a key press or release. `scancode` drives RDP, `keysym` drives
    /// VNC; each backend uses the form its protocol speaks.
    pub fn key(&self, scancode: u32, keysym: u32, down: bool, extended: bool) -> Result<(), Error> {
        if let Some(client) = self.rdp() {
            return client.key(scancode, down, extended);
        }
        match self.vnc() {
            Some(client) => client.key(keysym, down),
            None => Err(Error::State),
        }
    }

    pub fn pointer(&self, x: u16, y: u16, buttons: PointerButtons) -> Result<(), Error> {
        if let Some(client) = self.rdp() {
            return client.pointer(x, y, buttons.0);
        }
        match self.vnc() {
            Some(client) => client.pointer(x, y, buttons.0),
            None => Err(Error::State),
        }
    }

    /// Offers the local clipboard to the remote side, replacing whatever was offered
    /// before; the newest call wins. Each kind may appear once. Text over
    /// [`MAX_CLIPBOARD_TEXT_BYTES`] is cut here at the last whole character within
    /// it ([`cut_clipboard_text`]), sent, and reported as
    /// [`Notice::ClipboardTooLarge`] on the event stream once the session takes the
    /// group; it is never an error. An image over [`MAX_CLIPBOARD_IMAGE_BYTES`] is
    /// [`Error::Argument`], as is text that is not UTF-8 or a repeated kind. RDP
    /// offers every entry, VNC only the text. A connection with sharing off answers
    /// [`Error::Unavailable`].
    pub fn push_clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), Error> {
        if entries.len() > MAX_CLIPBOARD_ENTRIES {
            return Err(Error::Argument);
        }
        // A bad group is refused by both backends before anything is queued.
        let mut kinds = 0u32;
        for entry in entries {
            let bit = 1 << entry.kind.bit();
            if kinds & bit != 0 {
                return Err(Error::Argument);
            }
            kinds |= bit;
            match entry.kind {
                ClipboardKind::Text => {
                    if std::str::from_utf8(&entry.bytes).is_err() {
                        return Err(Error::Argument);
                    }
                }
                kind => {
                    if entry.bytes.is_empty() || entry.bytes.len() > kind.bound() {
                        return Err(Error::Argument);
                    }
                }
            }
        }
        if let Some(client) = self.rdp() {
            return client.push_clipboard(entries);
        }
        match self.vnc() {
            Some(client) => client.push_clipboard(entries),
            None => Err(Error::State),
        }
    }

    /// Offers clipboard text alone; shorthand for [`DesktopClient::push_clipboard`]
    /// with one text entry.
    pub fn clipboard(&self, text: &str) -> Result<(), Error> {
        self.push_clipboard(&[ClipboardEntry::text(text)])
    }

    /// Describes the settings an RDP attempt with this configuration would use, as
    /// `key=value` words. Diagnostic: it proves the clipboard and drive options
    /// reach the engine without a server.
    #[doc(hidden)]
    pub fn describe_rdp_settings(&self) -> Result<String, Error> {
        match self.rdp() {
            Some(client) => Ok(client.describe()),
            None => Err(Error::State),
        }
    }

    /// Describes the settings a VNC attempt with this configuration would use.
    #[doc(hidden)]
    pub fn describe_vnc_settings(&self) -> Result<String, Error> {
        match self.vnc() {
            Some(client) => Ok(client.describe()),
            None => Err(Error::State),
        }
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

/// The name the session reports for a status code.
///
/// The codes are the ones an attempt answers with: 0 ok, 1 argument, 2 unavailable,
/// 3 state, 4 aborted, 5 internal.
pub fn status_name(code: i32) -> String {
    match code {
        0 => "ok",
        1 => "argument",
        2 => "unavailable",
        3 => "state",
        4 => "aborted",
        5 => "internal",
        _ => "unknown",
    }
    .to_owned()
}

/// Runs single pieces of the engines without a session or a server: the text
/// encodings the backends use, the RDP clipboard channel, and the folder checks a
/// mapped drive makes. The functions behind them are the ones a session runs, not
/// copies, so a test proves the shipped behaviour on a machine with no server.
#[doc(hidden)]
pub mod probe {
    use super::Event;

    /// The formats one local copy offers on the cliprdr channel, and the bytes each
    /// carries, through the session's own conversion.
    pub fn rdp_clipboard_offer(entries: &[super::ClipboardEntry]) -> Vec<(u16, Vec<u8>)> {
        super::rdp::probe_offer(entries)
    }

    /// Runs the channel's offer path over `entries` and returns what it sent and the
    /// events it reported.
    pub fn rdp_clipboard_offer_events(entries: &[super::ClipboardEntry]) -> (Vec<u8>, Vec<Event>) {
        super::rdp::probe_offer_events(entries)
    }

    /// Plays canned CLIPRDR messages through the session's receive path after
    /// offering `offered`; returns the events emitted and the bytes replied with.
    pub fn rdp_clipboard_receive(
        offered: &[super::ClipboardEntry],
        messages: &[(u16, u16, Vec<u8>)],
    ) -> (Vec<Event>, Vec<u8>) {
        super::rdp::probe_receive(offered, messages)
    }

    /// UTF-8 as RDP's CF_UNICODETEXT: UTF-16LE, CRLF line ends, NUL terminated.
    /// This is the conversion an RDP session sends with.
    pub fn utf8_to_cf_unicode(text: &str) -> Vec<u8> {
        super::rdp::cf_unicode_text(text)
    }

    /// CF_UNICODETEXT bytes back to UTF-8, up to the first NUL; `None` when the
    /// text is not valid UTF-16. This is the conversion a session receives with.
    pub fn cf_unicode_to_utf8(bytes: &[u8]) -> Option<Vec<u8>> {
        super::rdp::text_from_cf_unicode(bytes)
    }

    /// UTF-8 as RFB Latin-1, or `None` when a character does not fit. This is the
    /// conversion a VNC session sends with.
    pub fn utf8_to_latin1(text: &str) -> Option<Vec<u8>> {
        super::vnc::utf8_to_latin1(text.as_bytes())
    }

    /// RFB Latin-1 as UTF-8, the conversion a VNC session receives with.
    pub fn latin1_to_utf8(bytes: &[u8]) -> Option<Vec<u8>> {
        Some(super::vnc::latin1_to_utf8(bytes))
    }

    /// One drive request as a server sends it: an IRP's major and minor function and
    /// its body after the common header. `on_file` sends it on the file the last
    /// successful create opened.
    pub struct DriveRequest<'a> {
        pub major: u32,
        pub minor: u32,
        pub on_file: bool,
        pub body: &'a [u8],
    }

    /// Runs the drive channel over `requests`, in order, against the folder
    /// `base`; returns the NTSTATUS each was answered with. This is the channel a
    /// session serves, not a copy of it.
    pub fn rdp_drive_requests(base: &str, requests: &[DriveRequest<'_>]) -> Option<Vec<u32>> {
        let folder = std::path::Path::new(base);
        let canned: Vec<super::rdp::ProbeRequest<'_>> = requests
            .iter()
            .map(|request| super::rdp::ProbeRequest {
                major: request.major,
                minor: request.minor,
                on_file: request.on_file,
                body: request.body,
            })
            .collect();
        Some(super::rdp::probe_requests(folder, &canned))
    }

    /// The same run as [`rdp_drive_requests`], with the data in each answer so a
    /// test can read a listing's entries across pages.
    pub fn rdp_drive_replies(
        base: &str,
        requests: &[DriveRequest<'_>],
    ) -> Option<Vec<(u32, Vec<u8>)>> {
        let folder = std::path::Path::new(base);
        let canned: Vec<super::rdp::ProbeRequest<'_>> = requests
            .iter()
            .map(|request| super::rdp::ProbeRequest {
                major: request.major,
                minor: request.minor,
                on_file: request.on_file,
                body: request.body,
            })
            .collect();
        Some(super::rdp::probe_replies(folder, &canned))
    }

    /// Runs the drive channel's device exchange against `base` with a server that
    /// answers `result`; returns the events it reported.
    pub fn rdp_drive_reply(base: &str, result: u32) -> Vec<Event> {
        super::rdp::probe_drive_reply(base, result)
    }

    /// Checks a folder the way an RDP attempt does before offering it as a drive:
    /// `Some(None)` when it can be offered, `Some(Some(problem))` when not, `None`
    /// without the RDP backend.
    pub fn rdp_drive_folder(path: &str) -> Option<Option<super::DriveProblem>> {
        Some(super::folder_problem(path))
    }
}

/// The name a session reports for its state.
pub fn state_name(state: State) -> String {
    match state {
        State::Idle => "idle",
        State::Connecting => "connecting",
        State::Connected => "connected",
        State::Closing => "closing",
        State::Closed => "closed",
        State::Failed(_) => "failed",
    }
    .to_owned()
}

/// The name a session reports for a failure.
pub fn failure_name(failure: Failure) -> String {
    match failure {
        Failure::None => "none",
        Failure::Connect => "connect",
        Failure::Tls => "tls",
        Failure::Authentication => "authentication",
        Failure::CertificateRejected => "certificate-rejected",
        Failure::UnsupportedSecurity => "unsupported-security",
        Failure::ProtocolFailure => "protocol",
        Failure::Io => "io",
        Failure::Cancelled => "cancelled",
        Failure::RemoteClosed => "remote-closed",
        Failure::Limit => "limit",
        Failure::Internal => "internal",
        Failure::Unknown(_) => "unknown",
    }
    .to_owned()
}

/// What an attempt would say about a folder offered as a drive.
///
/// The check a mapped folder gets before the server sees it: absolute, inside the
/// accessible file system, a directory that can be listed.
fn folder_problem(path: &str) -> Option<DriveProblem> {
    if path.is_empty() {
        return Some(DriveProblem::Missing);
    }
    if path.len() >= 1024 || path.encode_utf16().count() >= 1024 {
        return Some(DriveProblem::TooLong);
    }
    let lower = path.to_ascii_lowercase();
    let device_namespace = lower.starts_with(r"\\?\") || lower.starts_with(r"\\.\");
    if matches!(path, "*" | "%" | "DynamicDrives") || device_namespace {
        return Some(DriveProblem::NotAbsolute);
    }
    let candidate = std::path::Path::new(path);
    if !candidate.is_absolute() {
        return Some(DriveProblem::NotAbsolute);
    }
    match std::fs::metadata(candidate) {
        Ok(meta) if !meta.is_dir() => Some(DriveProblem::NotAFolder),
        Ok(_) => match std::fs::read_dir(candidate) {
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Some(DriveProblem::NoAccess)
            }
            Err(_) => Some(DriveProblem::Other(-1)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(DriveProblem::Missing),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Some(DriveProblem::NoAccess)
        }
        Err(_) => Some(DriveProblem::Missing),
    }
}

#[cfg(test)]
mod clipboard_cut_tests {
    use super::*;

    #[test]
    fn text_within_the_bound_is_left_whole() {
        assert_eq!(cut_clipboard_text(b"", 4), 0);
        assert_eq!(cut_clipboard_text(b"abcd", 4), 4);
        assert_eq!(cut_clipboard_text("é".as_bytes(), 2), 2);
    }

    #[test]
    fn the_cut_never_splits_a_character() {
        assert_eq!(cut_clipboard_text(b"abcde", 4), 4);
        // "aé": the bound falls inside the two-byte é, which is left out whole.
        assert_eq!(cut_clipboard_text("aé".as_bytes(), 2), 1);
        // Three- and four-byte characters.
        assert_eq!(cut_clipboard_text("a中文".as_bytes(), 5), 4);
        assert_eq!(cut_clipboard_text("a中文".as_bytes(), 6), 4);
        assert_eq!(cut_clipboard_text("😀😀".as_bytes(), 7), 4);
        assert_eq!(cut_clipboard_text("😀".as_bytes(), 3), 0);
        let text = "x".repeat(MAX_CLIPBOARD_TEXT_BYTES - 1) + "€";
        let len = cut_clipboard_text(text.as_bytes(), MAX_CLIPBOARD_TEXT_BYTES);
        assert_eq!(len, MAX_CLIPBOARD_TEXT_BYTES - 1);
        assert!(std::str::from_utf8(&text.as_bytes()[..len]).is_ok());
    }
}
