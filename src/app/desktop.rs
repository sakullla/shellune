//! Remote desktop sessions: the surface an RDP or VNC session draws (R8, R10,
//! R17-R20, R23).
//!
//! Three layers, kept apart so that the parts with rules can be tested without a
//! server, a window or the native libraries:
//!
//! * [DesktopTarget] - what a saved connection asks for, translated from the
//!   record and nothing else. It carries no password: the secret goes from the
//!   store into [NativeLink] and is never copied into a value the surface keeps.
//! * [DesktopSurface], [InputQueue] and [content_rect] - the framebuffer, the
//!   ordered input queue and the pixel geometry. They are pure data: a frame is
//!   copied in the order the protocol sent it, input leaves in the order the user
//!   produced it, and a pointer is mapped through the same fit the window draws.
//! * [DesktopLink] - the one seam to a protocol implementation. The native
//!   backend is [NativeLink]; a test drives the same seam with a script instead
//!   of a socket, which is what lets the ordering and release rules be proven on a
//!   machine that has no RDP or VNC server.
//!
//! Nothing here pretends an endpoint was verified. [availability] reports what
//! this build can really do, and a session whose protocol is not built says so
//! instead of opening a pane that could never show a picture.

use std::collections::VecDeque;
use std::time::Duration;

use gpui::{Bounds, ObjectFit, Pixels, px, size};
use shellune_core::{
    ConnectionRecord, ProtocolKind, RdpCertificatePolicy, SessionFailure, VncSecurity,
};
pub use shellune_desktop::{ClipboardEntry, ClipboardKind, DriveProblem, Notice};

/// How often a live surface looks for frames and state changes.
///
/// A desktop coalesces its own dirty regions, so this is only the interval at
/// which a batch is taken off the link and applied; 12ms is below the point where
/// moving a window feels delayed and far above the rate a remote desktop produces
/// regions at.
pub const DESKTOP_POLL_INTERVAL: Duration = Duration::from_millis(12);

/// The largest desktop edge this surface allocates for.
///
/// A server that announces an absurd size is refused instead of asking for a
/// framebuffer that could not be drawn: 8192 pixels on both axes is an 8K display,
/// which is already more than any endpoint this build targets.
pub const MAX_DESKTOP_EDGE: u32 = 8192;

/// Why a desktop link could not be created or used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// This build has no native backend for the protocol.
    Unavailable(String),
    /// The handle exists but is not in a state that accepts this call.
    State,
    /// The argument was refused before anything was sent.
    Argument,
    /// The protocol worker did not stop in time; its resources stay alive.
    Timeout,
    /// The backend reported an internal failure.
    Internal,
}

impl LinkError {
    /// The reason as the user's sentence.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Unavailable(reason) => format!("this build has no desktop backend ({reason})"),
            Self::State => "the desktop session is not in a state that accepts input".to_owned(),
            Self::Argument => "the desktop session refused that request".to_owned(),
            Self::Timeout => "the desktop worker did not stop in time".to_owned(),
            Self::Internal => "the desktop backend reported an internal failure".to_owned(),
        }
    }
}

/// Whether this build can open a protocol at all (R20, R23).
///
/// Read from the linked library itself, so a build without the native sources
/// reports the reason instead of offering a connection that would fail later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The backend is linked; the string is its own version report.
    Built(String),
    /// Nothing is linked; the string is why.
    Missing(String),
}

impl Availability {
    /// Whether a session of this protocol can be attempted.
    #[must_use]
    pub fn is_built(&self) -> bool {
        matches!(self, Self::Built(_))
    }

    /// The text a surface shows about the backend.
    #[must_use]
    pub fn report(&self) -> &str {
        match self {
            Self::Built(info) | Self::Missing(info) => info,
        }
    }
}

/// What a stored RDP or VNC connection asks for.
///
/// Built from the record so that the pane never reads a host file: the surface
/// knows a name, an address and the two policies, and the password is handed to
/// the link separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopTarget {
    protocol: ProtocolKind,
    name: String,
    address: String,
    port: u16,
    user: Option<String>,
    domain: Option<String>,
    certificate: RdpCertificatePolicy,
    security: VncSecurity,
    /// Whether the clipboard is shared with the remote side (R6).
    clipboard: bool,
    /// The local folder offered to a RDP server as a drive, when one is chosen.
    drive_folder: Option<String>,
}

impl DesktopTarget {
    /// The target of a saved connection, or None for a protocol that has no
    /// desktop surface.
    #[must_use]
    pub fn of_record(record: &ConnectionRecord) -> Option<Self> {
        match record {
            ConnectionRecord::Rdp(config) => Some(Self {
                protocol: ProtocolKind::Rdp,
                name: config.name().to_owned(),
                address: config.address().to_owned(),
                port: config.port().get(),
                user: config.user().map(str::to_owned),
                domain: config.domain().map(str::to_owned),
                certificate: config.certificate(),
                security: VncSecurity::Automatic,
                clipboard: config.clipboard(),
                drive_folder: config.drive_folder().map(str::to_owned),
            }),
            ConnectionRecord::Vnc(config) => Some(Self {
                protocol: ProtocolKind::Vnc,
                name: config.name().to_owned(),
                address: config.address().to_owned(),
                port: config.port().get(),
                user: None,
                domain: None,
                certificate: RdpCertificatePolicy::Ask,
                security: config.security(),
                clipboard: config.clipboard(),
                // A VNC server has no drive channel to offer a folder on.
                drive_folder: None,
            }),
            ConnectionRecord::Ssh(_) | ConnectionRecord::Serial(_) => None,
        }
    }

    /// The protocol this target speaks.
    #[must_use]
    pub fn protocol(&self) -> ProtocolKind {
        self.protocol
    }

    /// The saved name, for the tab and the status card.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What a status line says it is dialling.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    /// The address a link dials.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The port a link dials.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The account, when the record has one.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// The Windows domain, when the record has one.
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// What the connection does with a server certificate it has not been told to
    /// trust. Only RDP presents one.
    #[must_use]
    pub fn certificate(&self) -> RdpCertificatePolicy {
        self.certificate
    }

    /// How much this connection insists on encryption, for VNC.
    #[must_use]
    pub fn security(&self) -> VncSecurity {
        self.security
    }

    /// Whether this connection shares the clipboard with the remote side (R6).
    #[must_use]
    pub fn clipboard(&self) -> bool {
        self.clipboard
    }

    /// The local folder an RDP connection maps as a remote drive, if any (R5).
    #[must_use]
    pub fn drive_folder(&self) -> Option<&str> {
        self.drive_folder.as_deref()
    }

    /// Whether this connection asks for an encrypted transport.
    ///
    /// False for every RDP connection this build makes - the shim negotiates the
    /// standard RDP security - and false for a VNC connection set to the server's
    /// default, which may negotiate an unencrypted type. The surface reports
    /// exactly this and never labels such a session secure.
    #[must_use]
    pub fn requires_encryption(&self) -> bool {
        self.protocol == ProtocolKind::Vnc && self.security == VncSecurity::EncryptedOnly
    }
}

/// What the backend reported about one protocol.
#[must_use]
pub fn availability(protocol: ProtocolKind) -> Availability {
    let protocol = match protocol {
        ProtocolKind::Rdp => shellune_desktop::Protocol::Rdp,
        ProtocolKind::Vnc => shellune_desktop::Protocol::Vnc,
        // A local, SSH or serial session has no desktop backend to ask about.
        ProtocolKind::Ssh | ProtocolKind::Serial => {
            return Availability::Missing("this protocol has no desktop backend".to_owned());
        }
    };
    match shellune_desktop::backend_info(protocol) {
        Ok(info) => Availability::Built(info),
        Err(error) => Availability::Missing(error.to_string()),
    }
}

/// One dirty region, already copied out of the protocol library's buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    /// Left edge in desktop pixels.
    pub x: i32,
    /// Top edge in desktop pixels.
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// The desktop the region belongs to.
    pub desktop_width: u32,
    /// The desktop the region belongs to.
    pub desktop_height: u32,
    /// Bytes per row in pixels, which may exceed width * 4.
    pub stride: u32,
    /// BGRA pixels, height rows of stride bytes.
    pub pixels: Vec<u8>,
}

/// What a link reports. The variants are delivered in protocol order and are
/// applied in the order they arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkEvent {
    /// The session's state changed.
    State {
        /// Where the session is now.
        state: LinkState,
        /// Why it left the previous state, when it failed.
        failure: LinkFailure,
        /// A protocol detail number, for diagnostics only.
        detail: i32,
        /// The shim's own text, for the disclosure.
        reason: String,
    },
    /// A dirty region of the desktop.
    Frame(Region),
    /// The remote cursor can now be positioned locally.
    Pointer(shellune_desktop::PointerShape),
    /// The server asks whether its certificate is trusted.
    Certificate {
        /// Must be echoed with the answer.
        request_id: u32,
        /// The server that presented it.
        host: String,
        /// Certificate subject.
        subject: String,
        /// Certificate issuer.
        issuer: String,
        /// Fingerprint, shown before any decision.
        fingerprint: String,
        /// The shim's own flag word.
        flags: u32,
    },
    /// The peer copied something: every format of that one copy the backend could
    /// fetch, each kind at most once (R1-R3).
    Clipboard {
        /// The formats, text already decoded to UTF-8.
        entries: Vec<ClipboardEntry>,
    },
    /// Something the user should know that does not change the session's state: a
    /// clipboard the server refused, text that was cut, a folder that was not mapped.
    Notice(Notice),
}

/// Where a desktop session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Created, not dialled.
    Idle,
    /// Dialling, handshaking or authenticating.
    Connecting,
    /// Live: frames and input flow.
    Connected,
    /// Closing, either on request or because the peer went away.
    Closing,
    /// Stopped.
    Closed,
    /// Could not start, or stopped with this reason.
    Failed(LinkFailure),
}

/// Why a desktop session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFailure {
    /// Nothing failed.
    None,
    /// The address could not be reached.
    Connect,
    /// TLS negotiation failed.
    Tls,
    /// The credentials were refused.
    Authentication,
    /// The certificate was not accepted.
    CertificateRejected,
    /// The server offered only a security type this build cannot negotiate.
    UnsupportedSecurity,
    /// The protocol layer reported an error.
    Protocol,
    /// The transport reported an error.
    Io,
    /// The attempt was cancelled.
    Cancelled,
    /// The peer closed the session.
    RemoteClosed,
    /// A limit was hit.
    Limit,
    /// The shim reported an internal failure.
    Internal,
    /// A failure number this build does not know.
    Unknown(i32),
}

impl LinkFailure {
    /// The failure as the session record states it (R17).
    ///
    /// One projection, so a tab, a status card and a log line cannot disagree
    /// about why a session ended. The reason is the layer's own text and never
    /// carries a credential.
    #[must_use]
    pub fn session_failure(
        &self,
        reason: &str,
        protocol: ProtocolKind,
        target: &str,
        was_live: bool,
    ) -> SessionFailure {
        let detail = |fallback: &str| {
            if reason.trim().is_empty() {
                fallback.to_owned()
            } else {
                reason.to_owned()
            }
        };
        match self {
            Self::Authentication => SessionFailure::AuthenticationFailed,
            Self::CertificateRejected => SessionFailure::HostKeyRejected,
            Self::UnsupportedSecurity => SessionFailure::ConnectionFailed {
                reason: format!(
                    "the server offers only a security type this build cannot negotiate ({})",
                    detail("no detail")
                ),
            },
            Self::Tls => SessionFailure::ConnectionFailed {
                reason: format!("the encrypted transport failed: {}", detail("no detail")),
            },
            Self::Connect => SessionFailure::ConnectionFailed {
                reason: detail("the address could not be reached"),
            },
            Self::Cancelled => SessionFailure::ConnectionFailed {
                reason: "the connection attempt was cancelled".to_owned(),
            },
            Self::Limit => SessionFailure::DesktopIoFailed {
                protocol,
                host: target.to_owned(),
                reason: detail("the session reached a limit of this build"),
            },
            Self::RemoteClosed => SessionFailure::ConnectionLost,
            Self::Protocol | Self::Io | Self::Internal | Self::Unknown(_) => {
                if was_live {
                    SessionFailure::DesktopIoFailed {
                        protocol,
                        host: target.to_owned(),
                        reason: detail("the desktop session stopped"),
                    }
                } else {
                    SessionFailure::ConnectionFailed {
                        reason: detail("the desktop session could not start"),
                    }
                }
            }
            Self::None => SessionFailure::ConnectionLost,
        }
    }
}

/// The framebuffer one desktop session draws into.
///
/// The canvas is allocated once per negotiated size and every region is copied
/// into it in the order it arrived. A region that does not fit is refused rather
/// than written past the end: a wrong stride would otherwise be an out-of-bounds
/// write in this process.
#[derive(Default)]
pub struct DesktopSurface {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    dirty_tiles: Vec<bool>,
    applied: u64,
}

/// Limits each replacement texture to one MiB on a large desktop.
pub const DESKTOP_TILE: u32 = 512;

impl DesktopSurface {
    /// A surface with no picture yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The negotiated desktop size, or (0, 0) before the first frame.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Whether anything has been drawn.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.pixels.is_empty()
    }

    /// The BGRA canvas, height rows of width * 4 bytes.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// How many regions have been applied, which is what proves none was dropped.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn applied(&self) -> u64 {
        self.applied
    }

    /// Drops the picture, for a session that is starting again.
    pub fn reset(&mut self) {
        self.width = 0;
        self.height = 0;
        self.pixels.clear();
        self.dirty_tiles.clear();
        self.applied = 0;
    }

    /// Copies one region in. Returns whether the canvas accepted it.
    ///
    /// The size is checked first: a region that belongs to another desktop
    /// reallocates the canvas and its own pixels are then copied into it, which is
    /// what makes a resolution change after a resize show the new frame instead of
    /// stretching the old one.
    pub fn apply(&mut self, region: &Region) -> bool {
        let (width, height) = (region.desktop_width, region.desktop_height);
        if width == 0
            || height == 0
            || width > MAX_DESKTOP_EDGE
            || height > MAX_DESKTOP_EDGE
            || u64::from(width) * u64::from(height) * 4 > shellune_desktop::MAX_FRAME_BYTES as u64
            || region.width <= 0
            || region.height <= 0
        {
            return false;
        }
        let right = region.x.saturating_add(region.width);
        let bottom = region.y.saturating_add(region.height);
        if region.x < 0 || region.y < 0 || right > width as i32 || bottom > height as i32 {
            return false;
        }
        let row = region.width as usize * 4;
        let stride = region.stride as usize;
        if stride < row {
            return false;
        }
        let needed = (region.height as usize - 1)
            .saturating_mul(stride)
            .saturating_add(row);
        if region.pixels.len() < needed {
            return false;
        }
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            let bytes = width as usize * height as usize * 4;
            self.pixels = vec![0u8; bytes];
            self.dirty_tiles =
                vec![true; (width.div_ceil(DESKTOP_TILE) * height.div_ceil(DESKTOP_TILE)) as usize];
        }
        let canvas_stride = self.width as usize * 4;
        for line in 0..region.height as usize {
            let source = line * stride;
            let destination = (region.y as usize + line) * canvas_stride + region.x as usize * 4;
            self.pixels[destination..destination + row]
                .copy_from_slice(&region.pixels[source..source + row]);
            // RFB's fourth byte is padding, commonly zero. Remote desktop pixels
            // are opaque; interpreting that padding as alpha hides the picture.
            for pixel in self.pixels[destination..destination + row]
                .as_chunks_mut::<4>()
                .0
            {
                pixel[3] = 255;
            }
        }
        self.applied += 1;
        let columns = width.div_ceil(DESKTOP_TILE);
        for y in region.y as u32 / DESKTOP_TILE..=(bottom as u32 - 1) / DESKTOP_TILE {
            for x in region.x as u32 / DESKTOP_TILE..=(right as u32 - 1) / DESKTOP_TILE {
                self.dirty_tiles[(y * columns + x) as usize] = true;
            }
        }
        true
    }

    pub fn take_dirty_tiles(&mut self) -> impl Iterator<Item = usize> + '_ {
        self.dirty_tiles
            .iter_mut()
            .enumerate()
            .filter_map(|(i, dirty)| std::mem::take(dirty).then_some(i))
    }

    /// A packed BGRA tile from the composited surface.
    pub fn tile(&self, index: usize) -> (u32, u32, Vec<u8>) {
        let columns = self.width.div_ceil(DESKTOP_TILE);
        let x = index as u32 % columns * DESKTOP_TILE;
        let y = index as u32 / columns * DESKTOP_TILE;
        let w = DESKTOP_TILE.min(self.width - x);
        let h = DESKTOP_TILE.min(self.height - y);
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let at = ((row * self.width + x) * 4) as usize;
            pixels.extend_from_slice(&self.pixels[at..at + (w * 4) as usize]);
        }
        (w, h, pixels)
    }
}

/// One key the surface sent, in both forms a protocol may need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyCodes {
    /// PC set 1 scan code, which RDP carries.
    pub scancode: u32,
    /// X11 keysym, which VNC carries.
    pub keysym: u32,
    /// Whether the code needs the extended prefix.
    pub extended: bool,
    /// Whether producing this character needs Shift held on a US layout.
    pub shift: bool,
}

/// The mouse buttons, as one mask both protocols understand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons(u8);

impl Buttons {
    /// Left button.
    pub const LEFT: Buttons = Buttons(1);
    /// Middle button.
    pub const MIDDLE: Buttons = Buttons(2);
    /// Right button.
    pub const RIGHT: Buttons = Buttons(4);
    /// One wheel step up.
    pub const WHEEL_UP: Buttons = Buttons(8);
    /// One wheel step down.
    pub const WHEEL_DOWN: Buttons = Buttons(16);
    /// One wheel step left.
    pub const WHEEL_LEFT: Buttons = Buttons(32);
    /// One wheel step right.
    pub const WHEEL_RIGHT: Buttons = Buttons(64);

    /// No button at all.
    #[must_use]
    pub fn none() -> Self {
        Self(0)
    }

    /// This mask with another button added.
    #[must_use]
    pub fn with(self, other: Buttons) -> Buttons {
        Buttons(self.0 | other.0)
    }

    /// This mask without another button.
    #[must_use]
    pub fn without(self, other: Buttons) -> Buttons {
        Buttons(self.0 & !other.0)
    }

    /// Whether the mask holds a button.
    #[must_use]
    pub fn contains(self, other: Buttons) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the mask holds any of the three pointer buttons.
    #[must_use]
    pub fn any_button(self) -> bool {
        self.0 & (Self::LEFT.0 | Self::MIDDLE.0 | Self::RIGHT.0) != 0
    }
}

/// One thing the user asked the remote desktop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopInput {
    /// A key went down or up.
    Key {
        /// The code in both forms.
        codes: KeyCodes,
        /// Whether it is a press.
        down: bool,
    },
    /// The pointer moved, or a button changed.
    Pointer {
        /// Position on the remote desktop.
        x: u16,
        /// Position on the remote desktop.
        y: u16,
        /// The buttons held after this event.
        buttons: Buttons,
    },
    /// What this computer's clipboard holds, offered to the remote clipboard.
    Clipboard(Vec<ClipboardEntry>),
}

/// The remote desktop's input queue.
///
/// Everything the user does is queued and then sent in the order it happened: a
/// press cannot overtake the release before it, and a pointer move that arrives
/// between two button changes keeps its place. Releasing everything at once - on a
/// lost focus, a hidden window or a close - appends the releases to the same queue
/// instead of sending them out of order, so the remote side never sees a key held
/// after the session that held it is gone.
#[derive(Default)]
pub struct InputQueue {
    queued: VecDeque<DesktopInput>,
    keys: Vec<KeyCodes>,
    buttons: Buttons,
    /// Where the pointer was last put, kept across flushes so a release queued
    /// after the queue was sent goes to the same pixel instead of the corner.
    position: (u16, u16),
    motion_tail: bool,
}

impl InputQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many events are waiting to be sent.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued.len()
    }

    /// Whether nothing is waiting.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// The keys this queue believes are held down.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn held_keys(&self) -> &[KeyCodes] {
        &self.keys
    }

    /// The buttons this queue believes are held.
    #[must_use]
    pub fn held_buttons(&self) -> Buttons {
        self.buttons
    }

    /// Queues a key press or release.
    ///
    /// A shifted character on a US layout is sent as the base key with Shift held
    /// around it, which is what the remote side needs to produce the character the
    /// user typed; the pair keeps its order in the queue.
    pub fn push_key(&mut self, codes: KeyCodes, down: bool) {
        self.motion_tail = false;
        if down {
            let needs_shift = codes.shift && !self.shift_held();
            if needs_shift {
                self.queued.push_back(DesktopInput::Key {
                    codes: SHIFT_LEFT,
                    down: true,
                });
                self.keys.push(SHIFT_LEFT);
            }
            self.queued
                .push_back(DesktopInput::Key { codes, down: true });
            if !self.keys.contains(&codes) {
                self.keys.push(codes);
            }
            return;
        }
        self.queued
            .push_back(DesktopInput::Key { codes, down: false });
        if let Some(index) = self.keys.iter().rposition(|held| *held == codes) {
            self.keys.remove(index);
            // A Shift this queue pressed for a shifted character goes up with it.
            if codes.shift && self.keys.contains(&SHIFT_LEFT) {
                self.keys.retain(|held| *held != SHIFT_LEFT);
                self.queued.push_back(DesktopInput::Key {
                    codes: SHIFT_LEFT,
                    down: false,
                });
            }
        }
    }

    /// Holds or lets go of one modifier, so the remote side holds what the user
    /// holds here. Returns whether anything was queued: a modifier already in the
    /// wanted state adds nothing.
    ///
    /// A shortcut is a modifier held around a key, and the window reports the
    /// modifiers with the key rather than as keys of their own; this is how Ctrl+V
    /// reaches the remote side as Ctrl+V instead of a bare V (R4).
    pub fn set_modifier(&mut self, codes: KeyCodes, down: bool) -> bool {
        let held = self
            .keys
            .iter()
            .any(|key| key.scancode == codes.scancode && key.extended == codes.extended);
        if held == down {
            return false;
        }
        if down {
            self.push_key(codes, true);
        } else {
            self.motion_tail = false;
            self.queued
                .push_back(DesktopInput::Key { codes, down: false });
            self.keys
                .retain(|key| !(key.scancode == codes.scancode && key.extended == codes.extended));
        }
        true
    }

    fn shift_held(&self) -> bool {
        self.keys
            .iter()
            .any(|held| held.scancode == SHIFT_LEFT.scancode)
    }

    /// Queues a pointer state.
    pub fn push_pointer(&mut self, x: u16, y: u16, buttons: Buttons) {
        let motion = buttons == self.buttons;
        if motion
            && self.motion_tail
            && let Some(DesktopInput::Pointer {
                x: old_x, y: old_y, ..
            }) = self.queued.back_mut()
        {
            *old_x = x;
            *old_y = y;
            self.position = (x, y);
            return;
        }
        self.motion_tail = motion;
        self.buttons = buttons;
        self.position = (x, y);
        self.queued
            .push_back(DesktopInput::Pointer { x, y, buttons });
    }

    /// Queues one wheel step at a position, keeping the buttons that are held.
    pub fn push_wheel(&mut self, x: u16, y: u16, step: Buttons) {
        self.position = (x, y);
        self.motion_tail = false;
        self.queued.push_back(DesktopInput::Pointer {
            x,
            y,
            buttons: self.buttons.with(step),
        });
        self.queued.push_back(DesktopInput::Pointer {
            x,
            y,
            buttons: self.buttons,
        });
    }

    /// Queues what this computer's clipboard holds for the remote clipboard.
    ///
    /// It keeps its place among the keys: a paste shortcut queued after it reaches
    /// the remote side after the content it pastes.
    pub fn push_clipboard(&mut self, entries: Vec<ClipboardEntry>) {
        self.motion_tail = false;
        self.queued.push_back(DesktopInput::Clipboard(entries));
    }

    /// Appends the releases for everything the queue still believes is held.
    ///
    /// Returns how many events that added. Keys are released in the reverse order
    /// they went down, and the pointer is released last, so a remote side that
    /// tracks modifiers per key sees each one let go before the button that may
    /// have started a drag.
    pub fn release_all(&mut self) -> usize {
        self.motion_tail = false;
        if self.keys.is_empty() && !self.buttons.any_button() {
            return 0;
        }
        let mut added = 0;
        while let Some(codes) = self.keys.pop() {
            self.queued
                .push_back(DesktopInput::Key { codes, down: false });
            added += 1;
        }
        if self.buttons.any_button() {
            let (x, y) = self.position;
            self.buttons = Buttons::none();
            self.queued.push_back(DesktopInput::Pointer {
                x,
                y,
                buttons: self.buttons,
            });
            added += 1;
        }
        added
    }

    /// Sends everything that is waiting, in order.
    ///
    /// The queue is drained first and refilled only with what could not be sent,
    /// so a link that fails half way through keeps the rest in the order it had
    /// instead of losing it. Returns how many events reached the link.
    pub fn flush(&mut self, link: &dyn DesktopLink) -> Result<usize, LinkError> {
        let mut pending = std::mem::take(&mut self.queued);
        let mut sent = 0;
        let mut failure = None;
        while let Some(input) = pending.pop_front() {
            let result = match &input {
                DesktopInput::Key { codes, down } => link.key(*codes, *down),
                DesktopInput::Pointer { x, y, buttons } => link.pointer(*x, *y, *buttons),
                DesktopInput::Clipboard(entries) => link.clipboard(entries),
            };
            match result {
                Ok(()) => sent += 1,
                Err(error) => {
                    // Retry temporary backpressure. A permanently refused
                    // clipboard/argument must not block every later key forever.
                    if !matches!(error, LinkError::Unavailable(_) | LinkError::Argument) {
                        pending.push_front(input);
                    }
                    failure = Some(error);
                    break;
                }
            }
        }
        self.queued = pending;
        match failure {
            Some(error) => Err(error),
            None => Ok(sent),
        }
    }
}

/// The left Shift key, which a shifted character needs around it.
pub const SHIFT_LEFT: KeyCodes = KeyCodes {
    scancode: 0x2a,
    keysym: 0xffe1,
    extended: false,
    shift: false,
};

/// The left Control key, which a shortcut such as Ctrl+V needs held around it.
pub const CONTROL_LEFT: KeyCodes = KeyCodes {
    scancode: 0x1d,
    keysym: 0xffe3,
    extended: false,
    shift: false,
};

/// The left Alt key.
pub const ALT_LEFT: KeyCodes = KeyCodes {
    scancode: 0x38,
    keysym: 0xffe9,
    extended: false,
    shift: false,
};

/// The key codes for a key the window reported, or None for a key this build does
/// not translate.
///
/// The names are the ones GPUI produces - lower case letters, digits, the literal
/// character for punctuation, and the platform's name for everything else - so the
/// table is the one place the surface decides what a key means.
#[must_use]
pub fn key_codes(key: &str) -> Option<KeyCodes> {
    let named = |scancode: u32, keysym: u32, extended: bool| {
        Some(KeyCodes {
            scancode,
            keysym,
            extended,
            shift: false,
        })
    };
    match key {
        "escape" => return named(0x01, 0xff1b, false),
        "tab" => return named(0x0f, 0xff09, false),
        "enter" => return named(0x1c, 0xff0d, false),
        "backspace" => return named(0x0e, 0xff08, false),
        "delete" => return named(0x53, 0xffff, true),
        "insert" => return named(0x52, 0xff63, true),
        "home" => return named(0x47, 0xff50, true),
        "end" => return named(0x4f, 0xff57, true),
        "pageup" => return named(0x49, 0xff55, true),
        "pagedown" => return named(0x51, 0xff56, true),
        "left" => return named(0x4b, 0xff51, true),
        "right" => return named(0x4d, 0xff53, true),
        "up" => return named(0x48, 0xff52, true),
        "down" => return named(0x50, 0xff54, true),
        "space" => return named(0x39, 0x20, false),
        "capslock" => return named(0x3a, 0xffe5, false),
        "shift" => return Some(SHIFT_LEFT),
        "control" | "ctrl" => return Some(CONTROL_LEFT),
        "alt" | "option" => return Some(ALT_LEFT),
        "platform" | "cmd" | "super" | "win" | "meta" => {
            return named(0x5b, 0xffeb, true);
        }
        // The numeric keypad reports its own names, and RFB has its own keysyms
        // for them so a server does not have to guess from Num Lock.
        "numpad0" => return named(0x52, 0xffb0, false),
        "numpad1" => return named(0x4f, 0xffb1, false),
        "numpad2" => return named(0x50, 0xffb2, false),
        "numpad3" => return named(0x51, 0xffb3, false),
        "numpad4" => return named(0x4b, 0xffb4, false),
        "numpad5" => return named(0x4c, 0xffb5, false),
        "numpad6" => return named(0x4d, 0xffb6, false),
        "numpad7" => return named(0x47, 0xffb7, false),
        "numpad8" => return named(0x48, 0xffb8, false),
        "numpad9" => return named(0x49, 0xffb9, false),
        "numpadmultiply" => return named(0x37, 0xffaa, false),
        "numpadadd" => return named(0x4e, 0xffab, false),
        "numpadsubtract" => return named(0x4a, 0xffad, false),
        "numpaddecimal" => return named(0x53, 0xffae, false),
        "numpaddivide" => return named(0x35, 0xffaf, true),
        "numpadenter" => return named(0x1c, 0xff8d, true),
        _ => {}
    }
    if let Some(index) = (1..=12u32).find(|index| key == format!("f{index}")) {
        let scancode = match index {
            1..=10 => 0x3a + index,
            11 => 0x57,
            _ => 0x58,
        };
        return named(scancode, 0xffbd + index, false);
    }
    // Letters: the scan code is the keyboard's own order, and the keysym is the
    // character's ASCII value.
    let mut characters = key.chars();
    let character = characters.next()?;
    if characters.next().is_some() {
        return None;
    }
    let lower = character.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        const LETTERS: [(char, u32); 26] = [
            ('q', 0x10),
            ('w', 0x11),
            ('e', 0x12),
            ('r', 0x13),
            ('t', 0x14),
            ('y', 0x15),
            ('u', 0x16),
            ('i', 0x17),
            ('o', 0x18),
            ('p', 0x19),
            ('a', 0x1e),
            ('s', 0x1f),
            ('d', 0x20),
            ('f', 0x21),
            ('g', 0x22),
            ('h', 0x23),
            ('j', 0x24),
            ('k', 0x25),
            ('l', 0x26),
            ('z', 0x2c),
            ('x', 0x2d),
            ('c', 0x2e),
            ('v', 0x2f),
            ('b', 0x30),
            ('n', 0x31),
            ('m', 0x32),
        ];
        let scancode = LETTERS.iter().find(|(letter, _)| *letter == lower)?.1;
        return Some(KeyCodes {
            scancode,
            keysym: u32::from(character),
            extended: false,
            shift: character.is_ascii_uppercase(),
        });
    }
    // Digits and the unshifted punctuation of a US layout, plus the character each
    // one produces with Shift held.
    const PLAIN: [(&str, u32, u32); 21] = [
        ("1", 0x02, 0x21),
        ("2", 0x03, 0x40),
        ("3", 0x04, 0x23),
        ("4", 0x05, 0x24),
        ("5", 0x06, 0x25),
        ("6", 0x07, 0x5e),
        ("7", 0x08, 0x26),
        ("8", 0x09, 0x2a),
        ("9", 0x0a, 0x28),
        ("0", 0x0b, 0x29),
        ("-", 0x0c, 0x5f),
        ("=", 0x0d, 0x2b),
        ("[", 0x1a, 0x7b),
        ("]", 0x1b, 0x7d),
        (";", 0x27, 0x3a),
        ("'", 0x28, 0x22),
        ("`", 0x29, 0x7e),
        ("\\", 0x2b, 0x7c),
        (",", 0x33, 0x3c),
        (".", 0x34, 0x3e),
        ("/", 0x35, 0x3f),
    ];
    let text = character.to_string();
    for (plain, scancode, shifted) in PLAIN {
        if text == plain {
            return Some(KeyCodes {
                scancode,
                keysym: u32::from(character),
                extended: false,
                shift: false,
            });
        }
        if u32::from(character) == shifted {
            return Some(KeyCodes {
                scancode,
                keysym: u32::from(character),
                extended: false,
                shift: true,
            });
        }
    }
    None
}

/// How the picture is scaled into the space the window gives it (R18).
///
/// The ladder is fixed instead of free: every step is a value a user can name in
/// the status line, and Fit is the state a pane starts in. A step above Fit makes
/// the picture larger than the space it is drawn in, which is why a zoomed picture
/// is panned rather than silently cropped - see [content_rect_in].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DesktopZoom {
    /// The whole desktop, as large as it fits without cropping (the default).
    #[default]
    Fit,
    /// A fixed percentage of the remote pixels.
    Percent(u16),
}

impl DesktopZoom {
    /// Every step of the ladder, smallest first.
    pub const LADDER: [u16; 5] = [50, 75, 100, 150, 200];

    /// The next step in, when there is one.
    #[must_use]
    pub fn zoomed_in(self) -> Self {
        match self {
            // Coming from Fit, the largest step is the useful one: the user asked
            // for a bigger picture, not for the first rung of a ladder.
            Self::Fit => Self::Percent(Self::LADDER[Self::LADDER.len() - 1]),
            Self::Percent(current) => Self::Percent(
                Self::LADDER
                    .iter()
                    .copied()
                    .find(|step| *step > current)
                    .unwrap_or(current),
            ),
        }
    }

    /// The next step out, which is Fit once the smallest step is passed.
    #[must_use]
    pub fn zoomed_out(self) -> Self {
        match self {
            Self::Fit => Self::Fit,
            Self::Percent(current) => Self::LADDER
                .iter()
                .copied()
                .rev()
                .find(|step| *step < current)
                .map_or(Self::Fit, Self::Percent),
        }
    }

    /// Whether this is the fit-the-window step.
    #[must_use]
    pub fn is_fit(self) -> bool {
        matches!(self, Self::Fit)
    }

    /// The percentage of the remote pixels, when this step names one.
    #[must_use]
    pub fn percent(self) -> Option<u16> {
        match self {
            Self::Fit => None,
            Self::Percent(percent) => Some(percent),
        }
    }
}

/// The rectangle a desktop of that size fills inside a viewport.
///
/// Asked of GPUI's own ObjectFit, so the pixel the window draws and the pixel the
/// pointer is mapped through cannot disagree: the image is drawn with
/// ObjectFit::Contain, and this is that same fit's geometry. This is the Fit step
/// of [DesktopZoom].
#[must_use]
pub fn content_rect(viewport: Bounds<Pixels>, desktop: (u32, u32)) -> Bounds<Pixels> {
    if desktop.0 == 0 || desktop.1 == 0 {
        return viewport;
    }
    ObjectFit::Contain.get_bounds(
        viewport,
        size(
            gpui::DevicePixels(desktop.0 as i32),
            gpui::DevicePixels(desktop.1 as i32),
        ),
    )
}

/// The rectangle a desktop is drawn in at a zoom step, panned by that many pixels.
///
/// Fit is [content_rect]. A percentage draws the remote pixels at a fixed size and
/// centres the result in the viewport; a picture larger than the viewport is
/// cropped by it, and the pan is clamped so the picture always covers the viewport
/// and never leaves a gap beside itself.
#[must_use]
pub fn content_rect_in(
    viewport: Bounds<Pixels>,
    desktop: (u32, u32),
    zoom: DesktopZoom,
    pan: (f32, f32),
) -> Bounds<Pixels> {
    let DesktopZoom::Percent(percent) = zoom else {
        return content_rect(viewport, desktop);
    };
    if desktop.0 == 0 || desktop.1 == 0 {
        return viewport;
    }
    let factor = f32::from(percent) / 100.0;
    let width = desktop.0 as f32 * factor;
    let height = desktop.1 as f32 * factor;
    let (pan_x, pan_y) = clamp_pan(viewport, (width, height), pan);
    Bounds {
        origin: gpui::point(
            viewport.origin.x + (viewport.size.width - px(width)) / 2.0 + px(pan_x),
            viewport.origin.y + (viewport.size.height - px(height)) / 2.0 + px(pan_y),
        ),
        size: size(px(width), px(height)),
    }
}

/// How far a picture of that size may be panned inside a viewport.
///
/// The bound is the overhang on each axis: a picture smaller than the viewport
/// cannot move at all, and one larger than it covers the viewport everywhere
/// between the two limits. Half of it is the offset from the centred position.
#[must_use]
pub fn clamp_pan(viewport: Bounds<Pixels>, picture: (f32, f32), pan: (f32, f32)) -> (f32, f32) {
    let limit = |space: f32, extent: f32| ((extent - space).max(0.0)) / 2.0;
    let x = limit(f32::from(viewport.size.width), picture.0);
    let y = limit(f32::from(viewport.size.height), picture.1);
    (pan.0.clamp(-x, x), pan.1.clamp(-y, y))
}

/// The remote pixel under a window position, or None when the position is in the
/// letterbox around the picture.
///
/// This is the inverse of content_rect_in: the same offset and the same scale, so
/// a click on the drawn pixel reports the remote pixel that was drawn there. It
/// stays true at every zoom step and under any pan, which is what makes the
/// pointer usable while the picture is scaled or the window is full screen.
#[must_use]
pub fn to_remote_in(
    viewport: Bounds<Pixels>,
    desktop: (u32, u32),
    zoom: DesktopZoom,
    pan: (f32, f32),
    position: gpui::Point<Pixels>,
) -> Option<(u16, u16)> {
    if desktop.0 == 0 || desktop.1 == 0 || viewport.size.width <= px(0.0) {
        return None;
    }
    let rect = content_rect_in(viewport, desktop, zoom, pan);
    if !rect.contains(&position) {
        return None;
    }
    let scale_x = f32::from(rect.size.width) / desktop.0 as f32;
    let scale_y = f32::from(rect.size.height) / desktop.1 as f32;
    if scale_x <= 0.0 || scale_y <= 0.0 {
        return None;
    }
    let x = (f32::from(position.x) - f32::from(rect.origin.x)) / scale_x;
    let y = (f32::from(position.y) - f32::from(rect.origin.y)) / scale_y;
    let x = x.floor().clamp(0.0, (desktop.0 - 1) as f32) as u16;
    let y = y.floor().clamp(0.0, (desktop.1 - 1) as f32) as u16;
    Some((x, y))
}

/// The remote pixel under a window position at the Fit step.
#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub fn to_remote(
    viewport: Bounds<Pixels>,
    desktop: (u32, u32),
    position: gpui::Point<Pixels>,
) -> Option<(u16, u16)> {
    to_remote_in(viewport, desktop, DesktopZoom::Fit, (0.0, 0.0), position)
}

/// One protocol implementation, behind the seam the surface talks to.
///
/// Implemented by [NativeLink] over the shim, and by a script in the tests. The
/// surface never learns which one it holds.
pub trait DesktopLink: Send + 'static {
    /// Dials the endpoint. Called once, before any input.
    fn connect(&mut self) -> Result<(), LinkError>;
    /// Takes every event that arrived since the last call, in protocol order.
    fn drain(&mut self) -> Vec<LinkEvent>;
    /// Sends one key press or release.
    fn key(&self, codes: KeyCodes, down: bool) -> Result<(), LinkError>;
    /// Sends one pointer state.
    fn pointer(&self, x: u16, y: u16, buttons: Buttons) -> Result<(), LinkError>;
    /// Offers this computer's clipboard to the peer; the newest offer wins.
    fn clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), LinkError>;
    /// Answers a certificate question.
    fn answer_certificate(&self, request_id: u32, accept: bool) -> Result<(), LinkError>;
    /// Tells the protocol the window changed size.
    fn resize(&self, width: u32, height: u32) -> Result<(), LinkError>;
    /// Stops the worker and releases what it owns. Safe to call twice.
    fn shutdown(&mut self);
}

/// The native link: one shim handle and the queue it feeds.
pub struct NativeLink {
    client: shellune_desktop::DesktopClient,
}

impl NativeLink {
    /// Opens a handle for a target and dials it.
    ///
    /// The password is passed straight through and is not stored: an attempt that
    /// fails leaves no copy of it behind in this value.
    pub fn open(
        target: &DesktopTarget,
        password: Option<&str>,
        width: u32,
        height: u32,
    ) -> Result<Self, LinkError> {
        let protocol = match target.protocol() {
            ProtocolKind::Rdp => shellune_desktop::Protocol::Rdp,
            ProtocolKind::Vnc => shellune_desktop::Protocol::Vnc,
            _ => return Err(LinkError::Argument),
        };
        let mut client = shellune_desktop::DesktopClient::new(protocol).map_err(native_error)?;
        client
            .set_server(target.address(), target.port())
            .map_err(native_error)?;
        client
            .set_credentials(target.user(), password, target.domain())
            .map_err(native_error)?;
        client
            .set_size(width.max(1), height.max(1))
            .map_err(native_error)?;
        // The clipboard is shared unless the connection turned it off (R6); off
        // means the backend neither receives nor sends anything.
        client
            .set_clipboard(target.clipboard())
            .map_err(native_error)?;
        if target.protocol() == ProtocolKind::Rdp {
            // The folder is offered as one drive; a folder that cannot be offered
            // is reported as a notice and the session connects without it (R5).
            client
                .set_drive(shellune_desktop::DRIVE_NAME, target.drive_folder())
                .map_err(native_error)?;
        }
        if target.protocol() == ProtocolKind::Vnc {
            // The mask is what the connection insists on: a VNC connection set to
            // the server's default asks for the two types that need no TLS backend
            // and says so in the status; one set to encrypted-only asks for TLS and
            // VeNCrypt and fails instead of falling back to a weaker type.
            let mask = match target.security() {
                VncSecurity::Automatic => shellune_desktop::VncSecurity::DEFAULT,
                VncSecurity::EncryptedOnly => {
                    shellune_desktop::VncSecurity::TLS.with(shellune_desktop::VncSecurity::VENCRYPT)
                }
            };
            client.set_vnc_security(mask).map_err(native_error)?;
            // Four bytes per pixel is the only depth the shim delivers, and it says
            // so rather than silently accepting a depth it never produces.
            client.set_vnc_colour_depth(4).map_err(native_error)?;
        }
        client.connect().map_err(native_error)?;
        Ok(Self { client })
    }
}

impl DesktopLink for NativeLink {
    fn connect(&mut self) -> Result<(), LinkError> {
        // The handle was already dialled in open, which is the only place where the
        // address, the credentials and the size are known together.
        Ok(())
    }

    fn drain(&mut self) -> Vec<LinkEvent> {
        self.client
            .track_state()
            .into_iter()
            .filter_map(native_event)
            .collect()
    }

    fn key(&self, codes: KeyCodes, down: bool) -> Result<(), LinkError> {
        self.client
            .key(codes.scancode, codes.keysym, down, codes.extended)
            .map_err(native_error)
    }

    fn pointer(&self, x: u16, y: u16, buttons: Buttons) -> Result<(), LinkError> {
        let mut mask = shellune_desktop::PointerButtons(0);
        for (bit, button) in [
            (Buttons::LEFT, shellune_desktop::PointerButtons::LEFT),
            (Buttons::MIDDLE, shellune_desktop::PointerButtons::MIDDLE),
            (Buttons::RIGHT, shellune_desktop::PointerButtons::RIGHT),
            (
                Buttons::WHEEL_UP,
                shellune_desktop::PointerButtons::WHEEL_UP,
            ),
            (
                Buttons::WHEEL_DOWN,
                shellune_desktop::PointerButtons::WHEEL_DOWN,
            ),
            (
                Buttons::WHEEL_LEFT,
                shellune_desktop::PointerButtons::WHEEL_LEFT,
            ),
            (
                Buttons::WHEEL_RIGHT,
                shellune_desktop::PointerButtons::WHEEL_RIGHT,
            ),
        ] {
            if buttons.contains(bit) {
                mask = mask.with(button);
            }
        }
        self.client.pointer(x, y, mask).map_err(native_error)
    }

    fn clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), LinkError> {
        self.client.push_clipboard(entries).map_err(native_error)
    }

    fn answer_certificate(&self, request_id: u32, accept: bool) -> Result<(), LinkError> {
        self.client
            .answer_certificate(request_id, accept)
            .map_err(native_error)
    }

    fn resize(&self, _width: u32, _height: u32) -> Result<(), LinkError> {
        // The desktop size is taken before the connection is made and the handle is
        // owned by this value behind a mutable borrow, so a live session cannot be
        // renegotiated from here. Saying so lets the surface scale the picture it
        // already has instead of claiming a remote resize it never asked for.
        Err(LinkError::State)
    }

    fn shutdown(&mut self) {
        // Dropping the client cancels, detaches, waits and then frees; the handle is
        // not touched afterwards, so a second call has nothing left to do.
        let _ = self.client.cancel();
    }
}

impl Drop for NativeLink {
    fn drop(&mut self) {
        let _ = self.client.cancel();
    }
}

fn native_error(error: shellune_desktop::Error) -> LinkError {
    match error {
        shellune_desktop::Error::Unavailable(reason) => LinkError::Unavailable(reason),
        shellune_desktop::Error::Argument => LinkError::Argument,
        shellune_desktop::Error::State => LinkError::State,
        shellune_desktop::Error::Timeout => LinkError::Timeout,
        shellune_desktop::Error::Aborted => LinkError::State,
        shellune_desktop::Error::Internal | shellune_desktop::Error::Abi(_) => LinkError::Internal,
    }
}

fn native_event(event: shellune_desktop::Event) -> Option<LinkEvent> {
    Some(match event {
        shellune_desktop::Event::State {
            state,
            failure,
            detail,
            reason,
        } => LinkEvent::State {
            state: match state {
                shellune_desktop::State::Idle => LinkState::Idle,
                shellune_desktop::State::Connecting => LinkState::Connecting,
                shellune_desktop::State::Connected => LinkState::Connected,
                shellune_desktop::State::Closing => LinkState::Closing,
                shellune_desktop::State::Closed => LinkState::Closed,
                shellune_desktop::State::Failed(_) => LinkState::Failed(native_failure(failure)),
            },
            failure: native_failure(failure),
            detail,
            reason,
        },
        shellune_desktop::Event::Pointer(shape) => LinkEvent::Pointer(shape),
        shellune_desktop::Event::Frame {
            x,
            y,
            width,
            height,
            desktop_width,
            desktop_height,
            stride,
            pixels,
        } => LinkEvent::Frame(Region {
            x,
            y,
            width,
            height,
            desktop_width,
            desktop_height,
            stride,
            pixels,
        }),
        shellune_desktop::Event::Certificate {
            request_id,
            host,
            subject,
            issuer,
            fingerprint,
            flags,
        } => LinkEvent::Certificate {
            request_id,
            host,
            subject,
            issuer,
            fingerprint,
            flags,
        },
        shellune_desktop::Event::Clipboard { entries } => {
            if entries.is_empty() {
                return None;
            }
            LinkEvent::Clipboard { entries }
        }
        shellune_desktop::Event::Notice { notice } => LinkEvent::Notice(notice),
    })
}

fn native_failure(failure: shellune_desktop::Failure) -> LinkFailure {
    match failure {
        shellune_desktop::Failure::None => LinkFailure::None,
        shellune_desktop::Failure::Connect => LinkFailure::Connect,
        shellune_desktop::Failure::Tls => LinkFailure::Tls,
        shellune_desktop::Failure::Authentication => LinkFailure::Authentication,
        shellune_desktop::Failure::CertificateRejected => LinkFailure::CertificateRejected,
        shellune_desktop::Failure::UnsupportedSecurity => LinkFailure::UnsupportedSecurity,
        shellune_desktop::Failure::ProtocolFailure => LinkFailure::Protocol,
        shellune_desktop::Failure::Io => LinkFailure::Io,
        shellune_desktop::Failure::Cancelled => LinkFailure::Cancelled,
        shellune_desktop::Failure::RemoteClosed => LinkFailure::RemoteClosed,
        shellune_desktop::Failure::Limit => LinkFailure::Limit,
        shellune_desktop::Failure::Internal => LinkFailure::Internal,
        shellune_desktop::Failure::Unknown(code) => LinkFailure::Unknown(code),
    }
}

/// The text a status line adds about encryption (R19).
///
/// A VNC session set to the server's default is reported as what it is - a
/// connection that may negotiate an unencrypted type - and never as secure. Only a
/// connection that insisted on TLS or VeNCrypt is described as encrypted, and even
/// then only because it asked to be.
#[must_use]
pub fn encryption_note(target: &DesktopTarget) -> &'static str {
    match target.protocol() {
        ProtocolKind::Vnc if target.requires_encryption() => {
            "TLS or VeNCrypt required; an unencrypted server is refused"
        }
        ProtocolKind::Vnc => "not encrypted: the server's own security type is used",
        _ => "standard RDP security",
    }
}

/// Whether a surface holds a complete BGRA canvas for its size.
#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub fn canvas_is_bgra(surface: &DesktopSurface) -> bool {
    let (width, height) = surface.size();
    width > 0 && height > 0 && surface.pixels().len() == width as usize * height as usize * 4
}

/// A link whose events and inputs come from a script, for the tests that prove the
/// surface's ordering and release rules without an endpoint, and for the capture that
/// photographs the chrome of a session no server on this machine can produce.
#[cfg(any(test, feature = "ui-capture"))]
#[cfg_attr(not(test), allow(dead_code))]
pub mod scripted {
    use super::{
        Buttons, ClipboardEntry, DesktopInput, DesktopLink, KeyCodes, LinkError, LinkEvent, Region,
    };
    use std::sync::{Arc, Mutex};

    /// What a scripted link recorded.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Log {
        /// Every input the surface sent, in order.
        pub inputs: Vec<DesktopInput>,
        /// How often the worker was shut down.
        pub shutdowns: usize,
        /// Certificate answers as (request, accepted).
        pub answers: Vec<(u32, bool)>,
        /// Sizes the surface asked for.
        pub resizes: Vec<(u32, u32)>,
        /// Every clipboard offer the surface sent to the peer, in order.
        pub clipboard: Vec<Vec<ClipboardEntry>>,
    }

    /// The queue a scripted link reads, so a test can hand the pane more events
    /// after it has started - a second frame, a failure - the way a live peer does.
    #[derive(Clone)]
    pub struct ScriptedFeed {
        events: Arc<Mutex<Vec<LinkEvent>>>,
    }

    impl ScriptedFeed {
        /// Appends events to what the link will report on its next drain.
        pub fn feed(&self, mut events: Vec<LinkEvent>) {
            let mut queue = match self.events.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            queue.append(&mut events);
        }
    }

    /// A link backed by a queue and a log.
    pub struct ScriptedLink {
        events: Arc<Mutex<Vec<LinkEvent>>>,
        log: Arc<Mutex<Log>>,
        /// When set, every send fails with this error.
        fail: Option<LinkError>,
    }

    impl ScriptedLink {
        /// A link that will report these events, in this order.
        pub fn new(events: Vec<LinkEvent>) -> (Self, Arc<Mutex<Log>>) {
            let log = Arc::new(Mutex::new(Log::default()));
            (
                Self {
                    events: Arc::new(Mutex::new(events)),
                    log: log.clone(),
                    fail: None,
                },
                log,
            )
        }

        /// A link whose sends all fail, for the queue's retry rule.
        pub fn failing(error: LinkError) -> (Self, Arc<Mutex<Log>>) {
            let (mut link, log) = Self::new(Vec::new());
            link.fail = Some(error);
            (link, log)
        }

        /// A link that reports these events and then fails every send.
        pub fn failing_after(events: Vec<LinkEvent>, error: LinkError) -> (Self, Arc<Mutex<Log>>) {
            let (mut link, log) = Self::new(events);
            link.fail = Some(error);
            (link, log)
        }

        /// A handle that can add events after the pane has started.
        pub fn feed(&self) -> ScriptedFeed {
            ScriptedFeed {
                events: self.events.clone(),
            }
        }

        fn record(&self, input: DesktopInput) -> Result<(), LinkError> {
            if let Some(error) = &self.fail {
                return Err(error.clone());
            }
            let mut log = match self.log.lock() {
                Ok(log) => log,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let DesktopInput::Clipboard(entries) = &input {
                log.clipboard.push(entries.clone());
            }
            log.inputs.push(input);
            Ok(())
        }
    }

    impl DesktopLink for ScriptedLink {
        fn connect(&mut self) -> Result<(), LinkError> {
            Ok(())
        }

        fn drain(&mut self) -> Vec<LinkEvent> {
            match self.events.lock() {
                Ok(mut queue) => std::mem::take(&mut *queue),
                Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
            }
        }

        fn key(&self, codes: KeyCodes, down: bool) -> Result<(), LinkError> {
            self.record(DesktopInput::Key { codes, down })
        }

        fn pointer(&self, x: u16, y: u16, buttons: Buttons) -> Result<(), LinkError> {
            self.record(DesktopInput::Pointer { x, y, buttons })
        }

        fn clipboard(&self, entries: &[ClipboardEntry]) -> Result<(), LinkError> {
            self.record(DesktopInput::Clipboard(entries.to_vec()))
        }

        fn answer_certificate(&self, request_id: u32, accept: bool) -> Result<(), LinkError> {
            let mut log = match self.log.lock() {
                Ok(log) => log,
                Err(poisoned) => poisoned.into_inner(),
            };
            log.answers.push((request_id, accept));
            Ok(())
        }

        fn resize(&self, width: u32, height: u32) -> Result<(), LinkError> {
            let mut log = match self.log.lock() {
                Ok(log) => log,
                Err(poisoned) => poisoned.into_inner(),
            };
            log.resizes.push((width, height));
            Ok(())
        }

        fn shutdown(&mut self) {
            let mut log = match self.log.lock() {
                Ok(log) => log,
                Err(poisoned) => poisoned.into_inner(),
            };
            log.shutdowns += 1;
        }
    }

    /// A region of one colour, for the ordering tests.
    pub fn solid(
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        desktop: (u32, u32),
        bgra: [u8; 4],
    ) -> Region {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width * height {
            pixels.extend_from_slice(&bgra);
        }
        Region {
            x,
            y,
            width,
            height,
            desktop_width: desktop.0,
            desktop_height: desktop.1,
            stride: width as u32 * 4,
            pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn viewport(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(width), px(height)),
        }
    }

    #[test]
    fn a_letterboxed_picture_puts_the_remote_pixel_where_it_is_drawn() {
        let desktop = (1600, 900);
        let window = viewport(1000.0, 1000.0);
        let rect = content_rect(window, desktop);
        assert_eq!(f32::from(rect.size.width), 1000.0);
        assert_eq!(f32::from(rect.origin.y), 218.75);
        assert_eq!(
            to_remote(window, desktop, point(px(0.0), px(219.0))),
            Some((0, 0))
        );
        assert_eq!(
            to_remote(window, desktop, point(px(500.0), px(500.0))),
            Some((800, 450))
        );
        assert_eq!(to_remote(window, desktop, point(px(500.0), px(10.0))), None);
        assert_eq!(
            to_remote(window, desktop, point(px(500.0), px(990.0))),
            None
        );
    }

    #[test]
    fn a_pillarboxed_picture_maps_through_its_own_offset() {
        let desktop = (800, 1200);
        let window = viewport(1000.0, 1000.0);
        let rect = content_rect(window, desktop);
        assert!((f32::from(rect.origin.x) - 166.666_67).abs() < 0.001);
        assert_eq!(to_remote(window, desktop, point(px(100.0), px(0.0))), None);
        assert_eq!(
            to_remote(window, desktop, point(px(167.0), px(0.0))),
            Some((0, 0))
        );
        assert_eq!(
            to_remote(window, desktop, point(px(500.0), px(500.0))),
            Some((400, 600))
        );
    }

    #[test]
    fn the_zoom_ladder_steps_one_rung_at_a_time_and_fit_is_the_way_back() {
        assert_eq!(DesktopZoom::default(), DesktopZoom::Fit);
        assert_eq!(DesktopZoom::Fit.zoomed_in(), DesktopZoom::Percent(200));
        assert_eq!(
            DesktopZoom::Percent(200).zoomed_out(),
            DesktopZoom::Percent(150)
        );
        assert_eq!(
            DesktopZoom::Percent(100).zoomed_in(),
            DesktopZoom::Percent(150)
        );
        assert_eq!(
            DesktopZoom::Percent(50).zoomed_out(),
            DesktopZoom::Fit,
            "below the smallest step the picture goes back to fitting the window"
        );
        assert_eq!(
            DesktopZoom::Fit.zoomed_out(),
            DesktopZoom::Fit,
            "the fit step is where zooming out stops"
        );
        assert_eq!(
            DesktopZoom::Percent(200).zoomed_in(),
            DesktopZoom::Percent(200)
        );
        assert_eq!(DesktopZoom::Fit.percent(), None);
        assert_eq!(DesktopZoom::Percent(150).percent(), Some(150));
    }

    #[test]
    fn a_zoom_step_draws_the_remote_pixels_at_that_size_and_maps_them_back() {
        let desktop = (1600, 900);
        let window = viewport(800.0, 600.0);
        let rect = content_rect_in(window, desktop, DesktopZoom::Percent(200), (0.0, 0.0));
        assert_eq!(f32::from(rect.size.width), 3200.0);
        assert_eq!(f32::from(rect.size.height), 1800.0);
        assert_eq!(
            f32::from(rect.origin.x),
            -1200.0,
            "a picture larger than the window is centred on it"
        );
        assert_eq!(
            to_remote_in(
                window,
                desktop,
                DesktopZoom::Percent(200),
                (0.0, 0.0),
                point(px(400.0), px(300.0))
            ),
            Some((800, 450)),
            "the window centre is still the remote centre at 200%"
        );
    }

    #[test]
    fn panning_a_zoomed_picture_moves_the_pixel_the_pointer_names() {
        let desktop = (1600, 900);
        let window = viewport(800.0, 600.0);
        let zoom = DesktopZoom::Percent(200);
        assert_eq!(
            to_remote_in(
                window,
                desktop,
                zoom,
                (0.0, 0.0),
                point(px(400.0), px(300.0))
            ),
            Some((800, 450))
        );
        assert_eq!(
            to_remote_in(
                window,
                desktop,
                zoom,
                (200.0, 100.0),
                point(px(400.0), px(300.0))
            ),
            Some((700, 400)),
            "a picture moved right and down puts an earlier remote pixel under the same spot"
        );
        // The pan is bounded by the overhang, so the picture always covers the window.
        assert_eq!(
            clamp_pan(window, (3200.0, 1800.0), (5000.0, -5000.0)),
            (1200.0, -600.0)
        );
    }

    #[test]
    fn a_picture_smaller_than_the_window_cannot_be_panned_and_keeps_its_letterbox() {
        let desktop = (1600, 900);
        let window = viewport(800.0, 600.0);
        let zoom = DesktopZoom::Percent(50);
        let rect = content_rect_in(window, desktop, zoom, (100.0, 100.0));
        assert_eq!(f32::from(rect.size.width), 800.0);
        assert_eq!(f32::from(rect.size.height), 450.0);
        assert_eq!(f32::from(rect.origin.y), 75.0);
        assert_eq!(
            to_remote_in(
                window,
                desktop,
                zoom,
                (100.0, 100.0),
                point(px(400.0), px(10.0))
            ),
            None,
            "the letterbox around a zoomed-out picture is not part of the desktop"
        );
    }

    #[test]
    fn the_last_drawn_pixel_is_the_last_remote_pixel_and_never_past_it() {
        let desktop = (1600, 900);
        let window = viewport(1000.0, 1000.0);
        let rect = content_rect(window, desktop);
        let bottom = f32::from(rect.origin.y + rect.size.height) - 0.5;
        let right = f32::from(rect.size.width) - 0.5;
        assert_eq!(
            to_remote(window, desktop, point(px(right), px(bottom))),
            Some((1599, 899))
        );
    }

    #[test]
    fn a_picture_with_no_size_maps_nowhere() {
        assert_eq!(
            to_remote(viewport(800.0, 600.0), (0, 0), point(px(10.0), px(10.0))),
            None
        );
    }

    #[test]
    fn a_region_is_copied_in_the_order_it_arrived() {
        let mut surface = DesktopSurface::new();
        let desktop = (4, 4);
        let first = scripted::solid(0, 0, 4, 4, desktop, [1, 2, 3, 255]);
        let second = scripted::solid(1, 1, 2, 2, desktop, [9, 8, 7, 255]);
        assert!(surface.apply(&first));
        assert!(surface.apply(&second));
        assert_eq!(surface.applied(), 2);
        assert_eq!(surface.size(), desktop);
        assert_eq!(&surface.pixels()[0..4], &[1, 2, 3, 255]);
        let index = (4 + 1) * 4;
        assert_eq!(&surface.pixels()[index..index + 4], &[9, 8, 7, 255]);
        // The pixel the second region did not cover keeps the first region's value,
        // which is what proves nothing was coalesced past its own rectangle.
        assert_eq!(&surface.pixels()[0..4], &[1, 2, 3, 255]);
        assert!(canvas_is_bgra(&surface));
    }

    #[test]
    fn distant_changes_rebuild_only_two_4k_texture_tiles() {
        let mut surface = DesktopSurface::new();
        let desktop = (3840, 2160);
        let first = scripted::solid(0, 0, 16, 16, desktop, [1, 2, 3, 255]);
        surface.apply(&first);
        assert_eq!(surface.take_dirty_tiles().count(), 40);
        surface.apply(&first);
        surface.apply(&scripted::solid(
            3824,
            2144,
            16,
            16,
            desktop,
            [4, 5, 6, 255],
        ));
        let tiles: Vec<_> = surface.take_dirty_tiles().collect();
        assert_eq!(tiles, [0, 39]);
        let (w, h, corner) = surface.tile(39);
        assert_eq!((w, h), (256, 112));
        assert_eq!(&corner[corner.len() - 4..], &[4, 5, 6, 255]);
        assert_eq!(surface.tile(0).2.len() + corner.len(), 1_163_264);
        assert_eq!(surface.take_dirty_tiles().count(), 0);
    }

    #[test]
    fn a_region_that_does_not_fit_the_desktop_is_refused() {
        let mut surface = DesktopSurface::new();
        let desktop = (4, 4);
        assert!(!surface.apply(&scripted::solid(3, 0, 2, 1, desktop, [1, 1, 1, 255])));
        let mut narrow = scripted::solid(0, 0, 2, 1, desktop, [1, 1, 1, 255]);
        narrow.stride = 4;
        assert!(!surface.apply(&narrow));
        let mut short = scripted::solid(0, 0, 2, 2, desktop, [1, 1, 1, 255]);
        short.pixels.truncate(8);
        assert!(!surface.apply(&short));
        assert!(surface.is_empty());
        assert_eq!(surface.applied(), 0);
    }

    #[test]
    fn vnc_padding_is_not_rendered_as_transparency() {
        let mut surface = DesktopSurface::new();
        assert!(surface.apply(&scripted::solid(0, 0, 2, 1, (2, 1), [10, 20, 30, 0])));
        assert_eq!(surface.pixels(), &[10, 20, 30, 255, 10, 20, 30, 255]);
    }

    #[test]
    fn a_new_desktop_size_replaces_the_canvas() {
        let mut surface = DesktopSurface::new();
        assert!(surface.apply(&scripted::solid(0, 0, 2, 2, (2, 2), [4, 4, 4, 255])));
        assert_eq!(surface.size(), (2, 2));
        assert!(surface.apply(&scripted::solid(0, 0, 3, 3, (3, 3), [7, 7, 7, 255])));
        assert_eq!(surface.size(), (3, 3));
        assert_eq!(surface.pixels().len(), 3 * 3 * 4);
        assert_eq!(&surface.pixels()[0..4], &[7, 7, 7, 255]);
    }

    #[test]
    fn input_leaves_in_the_order_it_was_produced() {
        let (link, log) = scripted::ScriptedLink::new(Vec::new());
        let mut queue = InputQueue::new();
        let a = key_codes("a").expect("a is a key");
        let left = Buttons::none().with(Buttons::LEFT);
        queue.push_key(a, true);
        queue.push_pointer(10, 20, left);
        queue.push_key(a, false);
        queue.push_pointer(10, 20, Buttons::none());
        queue.push_wheel(10, 20, Buttons::WHEEL_UP);
        assert_eq!(queue.flush(&link), Ok(6));
        let log = log.lock().expect("the log is not poisoned").clone();
        assert_eq!(
            log.inputs,
            vec![
                DesktopInput::Key {
                    codes: a,
                    down: true
                },
                DesktopInput::Pointer {
                    x: 10,
                    y: 20,
                    buttons: left
                },
                DesktopInput::Key {
                    codes: a,
                    down: false
                },
                DesktopInput::Pointer {
                    x: 10,
                    y: 20,
                    buttons: Buttons::none()
                },
                DesktopInput::Pointer {
                    x: 10,
                    y: 20,
                    buttons: Buttons::WHEEL_UP
                },
                DesktopInput::Pointer {
                    x: 10,
                    y: 20,
                    buttons: Buttons::none()
                },
            ]
        );
    }

    #[test]
    fn delayed_pointer_motion_coalesces_without_moving_button_transitions() {
        let mut queue = InputQueue::new();
        let left = Buttons::LEFT;
        queue.push_pointer(1, 1, left);
        for x in 2..10_000 {
            queue.push_pointer(x, 2, left);
        }
        queue.push_pointer(10_000, 3, Buttons::none());
        queue.push_pointer(10_001, 4, Buttons::none());
        assert_eq!(queue.len(), 4);
        let (link, log) = scripted::ScriptedLink::new(Vec::new());
        queue.flush(&link).unwrap();
        let log = log.lock().unwrap();
        assert_eq!(
            log.inputs[0],
            DesktopInput::Pointer {
                x: 1,
                y: 1,
                buttons: left
            }
        );
        assert_eq!(
            log.inputs[1],
            DesktopInput::Pointer {
                x: 9999,
                y: 2,
                buttons: left
            }
        );
        assert_eq!(
            log.inputs[2],
            DesktopInput::Pointer {
                x: 10_000,
                y: 3,
                buttons: Buttons::none()
            }
        );
    }

    #[test]
    fn key_repeat_does_not_grow_held_key_state() {
        let mut queue = InputQueue::new();
        let a = key_codes("a").unwrap();
        for _ in 0..1000 {
            queue.push_key(a, true);
        }
        assert_eq!(queue.held_keys(), &[a]);
        queue.push_key(a, false);
        assert!(queue.held_keys().is_empty());
        assert_eq!(queue.release_all(), 0);
    }

    #[test]
    fn a_shifted_character_is_sent_with_shift_held_around_it() {
        let (link, log) = scripted::ScriptedLink::new(Vec::new());
        let mut queue = InputQueue::new();
        let bang = key_codes("!").expect("! is a key");
        assert!(bang.shift);
        assert_eq!(bang.scancode, 0x02, "! is shift-1 on a US layout");
        queue.push_key(bang, true);
        queue.push_key(bang, false);
        assert_eq!(queue.flush(&link), Ok(4));
        let log = log.lock().expect("the log is not poisoned").clone();
        let downs: Vec<_> = log
            .inputs
            .iter()
            .map(|input| match input {
                DesktopInput::Key { codes, down } => (*codes, *down),
                _ => panic!("only keys were queued"),
            })
            .collect();
        assert_eq!(downs[0], (SHIFT_LEFT, true));
        assert_eq!(downs[1], (bang, true));
        assert_eq!(downs[2], (bang, false));
        assert_eq!(downs[3], (SHIFT_LEFT, false));
    }

    #[test]
    fn losing_focus_releases_everything_that_was_held() {
        let (link, log) = scripted::ScriptedLink::new(Vec::new());
        let mut queue = InputQueue::new();
        let ctrl = key_codes("control").expect("control is a key");
        let shift = key_codes("shift").expect("shift is a key");
        let left = Buttons::none().with(Buttons::LEFT);
        queue.push_key(ctrl, true);
        queue.push_key(shift, true);
        queue.push_pointer(40, 50, left);
        let added = queue.release_all();
        assert_eq!(added, 3);
        assert!(queue.held_keys().is_empty());
        assert_eq!(queue.held_buttons(), Buttons::none());
        // The three events that were queued, and the three releases after them.
        assert_eq!(queue.flush(&link), Ok(6));
        let log = log.lock().expect("the log is not poisoned").clone();
        let tail = &log.inputs[3..];
        assert_eq!(
            tail,
            [
                DesktopInput::Key {
                    codes: shift,
                    down: false
                },
                DesktopInput::Key {
                    codes: ctrl,
                    down: false
                },
                DesktopInput::Pointer {
                    x: 40,
                    y: 50,
                    buttons: Buttons::none()
                },
            ]
        );
    }

    #[test]
    fn releasing_twice_adds_nothing() {
        let mut queue = InputQueue::new();
        queue.push_key(key_codes("a").expect("a is a key"), true);
        assert_eq!(queue.release_all(), 1);
        assert_eq!(queue.release_all(), 0);
    }

    #[test]
    fn an_event_that_fails_stays_at_the_front_of_the_queue() {
        let (link, _log) = scripted::ScriptedLink::failing(LinkError::State);
        let mut queue = InputQueue::new();
        let a = key_codes("a").expect("a is a key");
        queue.push_key(a, true);
        queue.push_key(a, false);
        assert_eq!(queue.flush(&link), Err(LinkError::State));
        assert_eq!(queue.len(), 2);
        let (working, log) = scripted::ScriptedLink::new(Vec::new());
        assert_eq!(queue.flush(&working), Ok(2));
        assert!(queue.is_empty());
        let log = log.lock().expect("the log is not poisoned").clone();
        assert_eq!(
            log.inputs[0],
            DesktopInput::Key {
                codes: a,
                down: true
            }
        );
        assert_eq!(
            log.inputs[1],
            DesktopInput::Key {
                codes: a,
                down: false
            }
        );
    }

    #[test]
    fn unsupported_clipboard_does_not_block_subsequent_keyboard_input() {
        let (link, _) = scripted::ScriptedLink::failing(LinkError::Unavailable("clipboard".into()));
        let mut queue = InputQueue::new();
        queue.push_clipboard(vec![ClipboardEntry::text("text")]);
        queue.push_key(key_codes("a").unwrap(), true);
        assert!(queue.flush(&link).is_err());
        assert_eq!(queue.len(), 1);
        let (working, log) = scripted::ScriptedLink::new(Vec::new());
        assert_eq!(queue.flush(&working), Ok(1));
        assert!(matches!(
            log.lock().unwrap().inputs[0],
            DesktopInput::Key { .. }
        ));
    }

    #[test]
    fn keys_the_build_does_not_know_are_not_sent_as_something_else() {
        assert!(key_codes("f13").is_none());
        assert!(key_codes("mediaplay").is_none());
        assert!(key_codes("").is_none());
        assert!(key_codes("aa").is_none());
        assert_eq!(key_codes("f1").map(|codes| codes.scancode), Some(0x3b));
        assert_eq!(key_codes("f12").map(|codes| codes.keysym), Some(0xffc9));
        assert_eq!(key_codes("left").map(|codes| codes.keysym), Some(0xff51));
        assert_eq!(key_codes("a").map(|codes| codes.scancode), Some(0x1e));
        assert_eq!(key_codes("A").map(|codes| codes.shift), Some(true));
    }

    #[test]
    fn a_target_carries_the_policies_and_never_a_secret() {
        let record = ConnectionRecord::Vnc(
            shellune_core::VncConfig::new("desk", "vnc.example.com").expect("a valid record"),
        );
        let target = DesktopTarget::of_record(&record).expect("a VNC record has a target");
        assert_eq!(target.protocol(), ProtocolKind::Vnc);
        assert_eq!(target.target(), "vnc.example.com:5900");
        assert!(!target.requires_encryption());
        assert!(encryption_note(&target).starts_with("not encrypted"));
    }

    #[test]
    fn a_connection_that_insists_on_encryption_is_allowed_to_say_so() {
        let mut config =
            shellune_core::VncConfig::new("desk", "vnc.example.com").expect("a valid record");
        config.set_security(VncSecurity::EncryptedOnly);
        let target = DesktopTarget::of_record(&ConnectionRecord::Vnc(config)).expect("a target");
        assert!(target.requires_encryption());
        assert!(encryption_note(&target).starts_with("TLS or VeNCrypt required"));
    }

    #[test]
    fn an_unsupported_security_type_is_a_failure_with_its_own_reason() {
        let target = "vnc.example.com:5901".to_owned();
        let failure = LinkFailure::UnsupportedSecurity.session_failure(
            "the server offered only security type 19",
            ProtocolKind::Vnc,
            &target,
            false,
        );
        let text = failure.to_string();
        assert!(
            text.contains("security type this build cannot negotiate"),
            "{text}"
        );
        assert!(failure.reconnect_can_help());
    }

    #[test]
    fn a_refused_credential_is_the_credential_problem_and_not_a_transport_one() {
        let failure =
            LinkFailure::Authentication.session_failure("", ProtocolKind::Rdp, "rdp:3389", false);
        assert_eq!(failure, SessionFailure::AuthenticationFailed);
        let rejected = LinkFailure::CertificateRejected.session_failure(
            "",
            ProtocolKind::Rdp,
            "rdp:3389",
            false,
        );
        assert_eq!(rejected, SessionFailure::HostKeyRejected);
    }

    #[test]
    fn a_live_session_that_stopped_is_not_reported_as_a_start_failure() {
        let live = LinkFailure::Protocol.session_failure(
            "read failed",
            ProtocolKind::Rdp,
            "rdp:3389",
            true,
        );
        assert_eq!(live.to_string(), "RDP · rdp:3389 · read failed");
        let vnc = LinkFailure::Io.session_failure(
            "os error 997",
            ProtocolKind::Vnc,
            "127.0.0.1:5901",
            true,
        );
        assert_eq!(vnc.to_string(), "VNC · 127.0.0.1:5901 · os error 997");
        let never = LinkFailure::Protocol.session_failure(
            "read failed",
            ProtocolKind::Rdp,
            "rdp:3389",
            false,
        );
        assert!(matches!(never, SessionFailure::ConnectionFailed { .. }));
    }

    #[test]
    fn the_build_reports_what_it_can_really_do() {
        let report = availability(ProtocolKind::Rdp);
        match &report {
            Availability::Built(info) => assert!(!info.is_empty()),
            Availability::Missing(reason) => assert!(!reason.is_empty()),
        }
        let ssh = availability(ProtocolKind::Ssh);
        assert!(!ssh.is_built());
    }
}
