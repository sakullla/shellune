//! One RDP attempt: negotiation, TLS/CredSSP, MCS, activation and the update loop.
//!
//! The socket is read with a short timeout so cancellation is observed without a
//! second reader. Every protocol failure becomes a `Failure` plus the text that
//! explains it; no failure is reported as a successful session.
use super::clipboard::Clipboard;
use super::drive::Drive;
use super::mcs::{self, Channels};
use super::security;
use super::share::{self, Share};
use super::update;
use super::wire::{self, Packet};
use super::{Command, Config, Control, Emit, publish};
use crate::input::Receiver;
use crate::{Event, Failure, State};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long the handshake may take before the attempt is abandoned.
const HANDSHAKE: Duration = Duration::from_secs(20);
/// A PDU arrives as a byte stream. A read that times out in the middle of one must
/// not lose what was already read, so bytes are kept here until a whole TPKT is
/// buffered; only then is it handed out.
#[derive(Default)]
struct Frames {
    pending: Vec<u8>,
    offset: usize,
}

impl Frames {
    fn push(&mut self, bytes: &[u8]) {
        if self.offset != 0 {
            self.pending.drain(..self.offset);
            self.offset = 0;
        }
        self.pending.extend_from_slice(bytes);
    }

    /// The next complete PDU body, when one is buffered.
    fn take(&mut self) -> io::Result<Option<Incoming>> {
        let pending = &self.pending[self.offset..];
        let [first, second, rest @ ..] = pending else {
            return Ok(None);
        };
        let slow = *first == 3;
        let (header, total) = if slow {
            if *second != 0 {
                return Err(wire::invalid("invalid TPKT version"));
            }
            let [hi, lo, ..] = rest else { return Ok(None) };
            (4, usize::from(u16::from_be_bytes([*hi, *lo])))
        } else {
            // Enhanced security is provided by TLS. Fast-path output has action 0
            // and a one- or two-byte length determinant including the header.
            if first & 0xc3 != 0 {
                return Err(wire::invalid("unsupported RDP fast-path output header"));
            }
            if second & 0x80 == 0 {
                (2, usize::from(*second))
            } else {
                let Some(lo) = rest.first() else {
                    return Ok(None);
                };
                (3, usize::from(u16::from_be_bytes([second & 0x7f, *lo])))
            }
        };
        if total < header {
            return Err(wire::invalid("short RDP pdu"));
        }
        if pending.len() < total {
            return Ok(None);
        }
        let body = pending[header..total].to_vec();
        self.offset += total;
        Ok(Some(if slow {
            Incoming::Slow(body)
        } else {
            Incoming::Fast(body)
        }))
    }
}

/// Reads from the transport until one complete PDU is buffered. A read that times
/// out in the middle of a PDU returns the error with the bytes kept, so the next
/// call continues the same PDU instead of parsing from its middle.
fn next_frame(transport: &mut impl Read, frames: &mut Frames) -> io::Result<Incoming> {
    loop {
        if let Some(frame) = read_chunk(transport, frames)? {
            return Ok(frame);
        }
    }
}

/// Give the input queue a turn after each socket read. A low-bandwidth peer
/// may stream one PDU for seconds without a single read timing out.
fn read_chunk(transport: &mut impl Read, frames: &mut Frames) -> io::Result<Option<Incoming>> {
    if let Some(frame) = frames.take()? {
        return Ok(Some(frame));
    }
    let mut chunk = [0u8; 16 * 1024];
    let read = transport.read(&mut chunk)?;
    if read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the server closed the connection",
        ));
    }
    frames.push(&chunk[..read]);
    frames.take()
}

/// How long the server has to confirm or refuse the logon. A server that says
/// nothing in this window is reported as unconfirmed rather than accepted.
const LOGON_WAIT: Duration = Duration::from_secs(10);
/// Maximum wait for the first decoded bitmap after activation.
const FIRST_PICTURE_WAIT: Duration = Duration::from_secs(60);
/// One read wait once the desktop is up: the loop returns to its cancellation and
/// deadline checks after each, and a partial record stays buffered.
const POLL: Duration = Duration::from_millis(32);

/// Ends one attempt with the business failure it maps to.
#[derive(Debug)]
struct Ended {
    failure: Failure,
    reason: String,
}

impl Ended {
    fn new(failure: Failure, reason: impl Into<String>) -> Ended {
        Ended {
            failure,
            reason: reason.into(),
        }
    }

    fn io(error: io::Error, during: Failure) -> Ended {
        let failure = match error.kind() {
            io::ErrorKind::PermissionDenied => Failure::Authentication,
            io::ErrorKind::Unsupported if during == Failure::Connect => {
                Failure::UnsupportedSecurity
            }
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                if during == Failure::Authentication =>
            {
                Failure::Io
            }
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset => Failure::RemoteClosed,
            _ if during != Failure::None => during,
            _ => Failure::Io,
        };
        Ended::new(failure, error.to_string())
    }
}

/// The slow-path keyboard flags of one key event (MS-RDPBCGR 2.2.8.1.2.2.1,
/// TS_KEYBOARD_EVENT).
///
/// These are not the fast-path bits: RELEASE is 0x8000 (not 0x01) and EXTENDED is
/// 0x0100 (not 0x02). A release sent with the fast-path bit reads as a press to the
/// server, so every character typed arrived twice - once on the way down and once on
/// the way up - until this was fixed.
fn keyboard_flags(down: bool, extended: bool) -> u16 {
    /// The key is going up; without it the server reads a press.
    const KBD_FLAGS_RELEASE: u16 = 0x8000;
    /// The key is one of the extended set (arrows, navigation, right-hand modifiers).
    const KBD_FLAGS_EXTENDED: u16 = 0x0100;
    let mut flags = 0;
    if !down {
        flags |= KBD_FLAGS_RELEASE;
    }
    if extended {
        flags |= KBD_FLAGS_EXTENDED;
    }
    flags
}

enum Transport {
    Plain(TcpStream),
    Secure(Box<security::SecureStream<Transport>>),
}

impl Read for Transport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Transport::Plain(stream) => stream.read(buffer),
            Transport::Secure(stream) => stream.read(buffer),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Transport::Plain(stream) => stream.write(buffer),
            Transport::Secure(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Transport::Plain(stream) => stream.flush(),
            Transport::Secure(stream) => stream.flush(),
        }
    }
}

enum Incoming {
    Slow(Vec<u8>),
    Fast(Vec<u8>),
}

/// Runs one attempt and reports how it ended.
///
/// A session the server drops is reported as ended, with the reason the layer gave;
/// the client does not dial again on its own. A silent second dial is a second logon
/// to the server - on xrdp it reconnects to the same display and takes it over from
/// the first client - and it hides the drop from the person who could decide to
/// reconnect. Reconnecting is an explicit action of the session, which opens a new
/// attempt (a new pane and a new link).
pub(super) fn run(
    config: &Config,
    emit: &Emit,
    control: &Arc<Control>,
    commands: Receiver<Command>,
) {
    if control.cancelled() {
        return;
    }
    super::trace("starting connection");
    match connect(config, emit, control, &commands) {
        Ok(mut active) => {
            // The pump publishes Connected, and only once the logon is settled:
            // a refused set of credentials must never look like a desktop.
            match active.pump(emit, control, &commands) {
                Ok(()) => publish(emit, control, &State::Closed, Failure::None, ""),
                Err(ended) => {
                    if control.cancelled() {
                        publish(emit, control, &State::Closed, Failure::None, "");
                        return;
                    }
                    publish(
                        emit,
                        control,
                        &State::Failed(ended.failure),
                        ended.failure,
                        &ended.reason,
                    );
                }
            }
        }
        Err(ended) => {
            if control.cancelled() {
                publish(emit, control, &State::Closed, Failure::None, "");
                return;
            }
            publish(
                emit,
                control,
                &State::Failed(ended.failure),
                ended.failure,
                &ended.reason,
            );
        }
    }
}

/// Some Windows sessions clear the surface during activation, before the
/// signed-in desktop is available. Keep showing every bitmap, including black.
#[derive(Default, PartialEq, Eq)]
enum StartupPicture {
    #[default]
    Empty,
    Black,
    RedrawAfter(Instant),
    RedrawSent,
    Content,
}

impl StartupPicture {
    fn observe(&mut self, pixels: &[u8]) {
        if *self == Self::Content || (*self == Self::RedrawSent && !super::trace_enabled()) {
            return;
        }
        if pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] | p[1] | p[2] != 0)
        {
            super::trace("first non-black desktop pixels decoded");
            *self = Self::Content;
        } else if *self == Self::Empty {
            super::trace("first bitmap is black");
            *self = Self::Black;
        }
    }

    /// Allow updates already on their way to arrive before requesting a repaint.
    /// At most one additional refresh per activation, even for a genuinely black desktop.
    fn redraw_due(&mut self, now: Instant) -> bool {
        match self {
            Self::Black => {
                *self = Self::RedrawAfter(now + Duration::from_millis(200));
                false
            }
            Self::RedrawAfter(deadline) if now >= *deadline => {
                *self = Self::RedrawSent;
                true
            }
            _ => false,
        }
    }
}

struct Active {
    transport: Transport,
    channels: Channels,
    share: Share,
    logon_confirmed: bool,
    /// Bytes of a PDU that has not fully arrived yet.
    frames: Frames,
    /// Whether Connected has been published.
    connected: bool,
    /// When an unconfirmed logon stops being waited for.
    logon_deadline: std::time::Instant,
    first_picture_deadline: Option<std::time::Instant>,
    startup_picture: StartupPicture,
    deactivated: bool,
    clipboard: Clipboard,
    drive: Drive,
    /// The button state the host last reported, so each edge carries its button.
    buttons: u32,
    pointer_position: Option<(u16, u16)>,
    pointers: super::pointer::Pointers,
    fast_fragment: Option<(u8, Vec<u8>)>,
    display: super::display::Display,
}

fn connect(
    config: &Config,
    emit: &Emit,
    control: &Arc<Control>,
    commands: &Receiver<Command>,
) -> Result<Active, Ended> {
    let address = format!("{}:{}", config.host, config.port);
    let endpoints = address
        .to_socket_addrs()
        .map_err(|error| Ended::io(error, Failure::Connect))?;
    let mut last = Ended::new(Failure::Connect, "no address resolved for the RDP server");
    let mut socket = None;
    for endpoint in endpoints {
        match TcpStream::connect_timeout(&endpoint, HANDSHAKE) {
            Ok(stream) => {
                socket = Some(stream);
                break;
            }
            Err(error) => last = Ended::io(error, Failure::Connect),
        }
    }
    let socket = socket.ok_or(last)?;
    socket.set_nodelay(true).ok();
    socket
        .set_read_timeout(Some(HANDSHAKE))
        .map_err(|error| Ended::io(error, Failure::Connect))?;
    control.attach(&socket);

    let mut transport = Transport::Plain(socket);
    // The X.224 negotiation only offers external security; the old standard RDP
    // security is never selected.
    let selected = wire::negotiate(&mut transport, true)
        .map_err(|error| Ended::io(error, Failure::Connect))?;
    super::trace(&format!("negotiated security {selected:?}"));

    if selected != wire::Security::Tls && (config.user.is_empty() || config.password.is_empty()) {
        return Err(Ended::new(
            Failure::CredentialsRequired,
            "Windows requires an account and password",
        ));
    }
    let verify = {
        let control = Arc::clone(control);
        let emit = Arc::clone(emit);
        let host = config.host.clone();
        Arc::new(
            move |subject: String, issuer: String, fingerprint: String| {
                control.ask(&emit, &host, subject, issuer, fingerprint)
            },
        ) as Arc<security::Ask>
    };
    super::trace("tls handshake starting");
    let mut secure = security::tls(
        transport,
        &config.host,
        config.certificate_pin.clone(),
        verify,
    )
    .map_err(|error| {
        if error.to_string().contains(security::CERTIFICATE_REJECTED) {
            Ended::new(
                Failure::CertificateRejected,
                "the RDP certificate was rejected",
            )
        } else {
            Ended::io(error, Failure::Tls)
        }
    })?;
    super::trace("tls handshake done");
    security::authenticate(
        &mut secure,
        selected,
        &config.host,
        &config.user,
        &config.domain,
        &config.password,
    )
    .map_err(|error| Ended::io(error, Failure::Authentication))?;
    super::trace("authentication done");
    let mut transport = Transport::Secure(Box::new(secure));

    if control.cancelled() {
        return Err(Ended::new(Failure::Cancelled, "cancelled"));
    }
    let channels = mcs::connect(
        &mut transport,
        config.width,
        config.height,
        config.scale_percent,
        selected,
        config.clipboard,
        config.drive.is_some(),
    )
    .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
    super::trace("mcs connected");

    share::login(
        &channels,
        &mut transport,
        &config.user,
        &config.domain,
        &config.password,
        selected,
    )
    .map_err(|error| Ended::io(error, Failure::Authentication))?;
    super::trace("login pdu sent");

    let mut active = Active {
        transport,
        channels,
        share: Share::pending(config.width, config.height),
        clipboard: Clipboard::new(config.clipboard, Arc::clone(emit)),
        drive: Drive::new(config.drive.clone(), Arc::clone(emit)),
        buttons: 0,
        pointer_position: None,
        pointers: Default::default(),
        fast_fragment: None,
        display: Default::default(),
        first_picture_deadline: None,
        startup_picture: StartupPicture::default(),
        deactivated: false,
        logon_confirmed: false,
        frames: Frames::default(),
        connected: false,
        logon_deadline: std::time::Instant::now() + LOGON_WAIT,
    };
    active.await_activation(control)?;
    // A synchronize event is how a client reports the keyboard toggle state;
    // servers expect it before forwarding input.
    active
        .share
        .input(&active.channels, &mut active.transport, 0, 0, 0, 0)
        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
    super::trace("desktop activated");
    // From here on the loop must come back often enough to notice a cancel and to
    // decide that a logon was never confirmed.
    active.first_picture_deadline = Some(std::time::Instant::now() + FIRST_PICTURE_WAIT);
    active.set_poll_timeout(POLL);
    let _ = commands;
    Ok(active)
}

/// Set Error Info carries a disconnect reason from any stage (MS-RDPBCGR
/// 2.2.5.1.1). Only Save Session Info confirms the logon here.
fn logon_pdu(kind: u8, payload: &[u8]) -> Result<bool, Ended> {
    if kind != 47 {
        return Ok(kind == 38);
    }
    let code = Packet(payload)
        .le32()
        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
    let (failure, reason) = match code {
        0 => return Ok(false), // ERRINFO_NONE
        0x0007 => (Failure::Connect, "the RDP server denied the connection"),
        0x0009 => (
            Failure::Authentication,
            "the account lacks remote desktop access",
        ),
        0x000a => (
            Failure::Authentication,
            "the RDP server requires fresh credentials",
        ),
        0x10ea => (
            Failure::ProtocolFailure,
            "the RDP server rejected the client capabilities in Confirm Active",
        ),
        0x1000..=0x1fff => (
            Failure::ProtocolFailure,
            "the RDP server reported a protocol error",
        ),
        _ => (Failure::RemoteClosed, "the RDP server ended the connection"),
    };
    Err(Ended::new(
        failure,
        format!("{reason} (code {code}, 0x{code:08X})"),
    ))
}

impl Active {
    /// Reads until the server activates the desktop, then acknowledges it. The wait
    /// is bounded: a server that never activates is a failed attempt, not a hang.
    fn await_activation(&mut self, control: &Arc<Control>) -> Result<(), Ended> {
        let deadline = std::time::Instant::now() + HANDSHAKE;
        loop {
            if control.cancelled() {
                return Err(Ended::new(Failure::Cancelled, "cancelled"));
            }
            if std::time::Instant::now() > deadline {
                return Err(Ended::new(
                    Failure::Io,
                    "the server did not activate the desktop in time",
                ));
            }
            let Some(body) = self.read_slow()? else {
                continue;
            };
            let Some((kind, header)) = self.control(&body) else {
                continue;
            };
            if kind != 1 {
                continue;
            }
            let share = Share::from_demand(&header)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            share
                .activate(&self.channels, &mut self.transport)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            self.share = share;
            self.pointer_position = None;
            return Ok(());
        }
    }

    /// Sets a short read wait on the socket under the TLS layer, so the loop
    /// returns to its cancellation and deadline checks between updates.
    fn set_poll_timeout(&self, wait: Duration) {
        match &self.transport {
            Transport::Plain(socket) => {
                let _ = socket.set_read_timeout(Some(wait));
            }
            Transport::Secure(secure) => {
                if let Transport::Plain(socket) = &secure.sock {
                    let _ = socket.set_read_timeout(Some(wait));
                }
            }
        }
    }

    fn announce_logon(&mut self, emit: &Emit, control: &Control) {
        if self.connected {
            return;
        }
        let reason = if self.logon_confirmed {
            ""
        } else if std::time::Instant::now() > self.logon_deadline {
            "logon not confirmed: the server authenticates through its own screen"
        } else {
            return;
        };
        publish(emit, control, &State::Connected, Failure::None, reason);
        self.connected = true;
    }

    fn pump(
        &mut self,
        emit: &Emit,
        control: &Arc<Control>,
        commands: &Receiver<Command>,
    ) -> Result<(), Ended> {
        loop {
            if control.cancelled() {
                let _ = self.drive.close(&self.channels, &mut self.transport);
                return Err(Ended::new(Failure::Cancelled, "cancelled"));
            }
            if self
                .first_picture_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            {
                return Err(Ended::new(
                    Failure::Io,
                    "the RDP server did not send a desktop picture within 60 seconds of activation",
                ));
            }
            self.announce_logon(emit, control);
            if !self.deactivated
                && self.logon_confirmed
                && self.share.refresh_rects
                && self.startup_picture.redraw_due(Instant::now())
            {
                self.share
                    .refresh(&self.channels, &mut self.transport)
                    .map_err(|e| Ended::io(e, Failure::ProtocolFailure))?;
                super::trace("requested post-logon repaint of black desktop");
            }
            if !self.deactivated {
                self.drain(commands)?;
            }
            self.display
                .poll(&self.channels, &mut self.transport)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            match read_chunk(&mut self.transport, &mut self.frames) {
                Ok(None) => continue,
                Ok(Some(Incoming::Slow(body))) => {
                    self.slow(&body, emit)?;
                }
                Ok(Some(Incoming::Fast(body))) => {
                    self.fast(&body, emit)
                        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(Ended::io(error, Failure::RemoteClosed)),
            }
        }
    }

    /// Applies every queued input or clipboard command before the next read.
    fn drain(&mut self, commands: &Receiver<Command>) -> Result<(), Ended> {
        for _ in 0..64 {
            match commands.pop() {
                Some(Command::Key {
                    scancode,
                    down,
                    extended,
                }) => self
                    .key(scancode, down, extended)
                    .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?,
                Some(Command::Pointer { buttons, x, y }) => {
                    let flags = super::pointer_flags(self.buttons, buttons);
                    if (self.buttons & 7 == 0) != (buttons & 7 == 0) {
                        // Service drag motion even while the server is waiting for
                        // input before repainting. Restore the idle wait on release.
                        self.set_poll_timeout(if buttons & 7 != 0 {
                            Duration::from_millis(8)
                        } else {
                            POLL
                        });
                    }
                    self.buttons = buttons;
                    if flags & 0x0400 != 0 && !self.share.horizontal_wheel {
                        continue;
                    }
                    // Position the cursor before a button edge or wheel step.
                    let moved = self.pointer_position != Some((x, y));
                    if moved && flags != 0x0800 {
                        self.pointer(0x0800, x, y)
                            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                    }
                    if moved || flags != 0x0800 {
                        self.pointer(flags, x, y)
                            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                    }
                    self.pointer_position = Some((x, y));
                }
                Some(Command::Resize(w, h, percent)) => self.display.resize(w, h, percent),
                Some(Command::Clipboard(entries)) => self
                    .clipboard
                    .offer(&self.channels, &mut self.transport, entries)
                    .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?,
                None => break,
            }
        }
        Ok(())
    }

    /// Reads bytes until a whole PDU is buffered. A timeout part-way through keeps
    /// what arrived, so the next call continues the same PDU instead of starting in
    /// the middle of it.
    fn read(&mut self) -> io::Result<Incoming> {
        next_frame(&mut self.transport, &mut self.frames)
    }

    fn read_slow(&mut self) -> Result<Option<Vec<u8>>, Ended> {
        match self.read() {
            Ok(Incoming::Slow(body)) => Ok(Some(body)),
            Ok(Incoming::Fast(_)) => Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(Ended::io(error, Failure::ProtocolFailure)),
        }
    }

    /// Strips the X.224 and MCS headers and returns the Share Control header and
    /// its body when the PDU belongs to the global channel. Anything else (a
    /// static channel message, another MCS primitive) is not a share control PDU
    /// and is reported as absent, never guessed at.
    fn control<'a>(&'a self, body: &'a [u8]) -> Option<(u16, share::Control<'a>)> {
        let mut p = Packet(body);
        if p.u8().ok()? != 2 {
            return None;
        }
        p.take(2).ok()?;
        let payload = if p.0.first() == Some(&0x68) {
            match self.channels.receive(p.0).ok()? {
                (id, payload) if id == self.channels.global => payload,
                _ => return None,
            }
        } else {
            p.0
        };
        let mut p = Packet(payload);
        let control = share::control(&mut p).ok()?;
        Some((control.kind, control))
    }

    fn slow(&mut self, body: &[u8], emit: &Emit) -> Result<(), Ended> {
        let mut p = Packet(body);
        if p.u8()
            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?
            != 2
        {
            return Ok(());
        }
        p.take(2)
            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
        if p.0.first() != Some(&0x68) {
            // Anything but an MCS SendDataIndication is not a share message this
            // client renders; the X.224 layer owns it.
            return Ok(());
        }
        // The MCS opcode is the same on every channel, so the channel ID decides
        // what the body is: a static channel's own data, or the global channel's
        // Share Control PDUs.
        let (id, payload) = self
            .channels
            .receive(p.0)
            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
        if Some(id) == self.channels.dynamic {
            return self
                .display
                .channel(&self.channels, &mut self.transport, id, payload)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure));
        }
        if Some(id) == self.channels.clipboard {
            self.clipboard
                .receive(&self.channels, &mut self.transport, payload)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            return Ok(());
        }
        if Some(id) == self.channels.drive {
            self.drive
                .on_data(&self.channels, &mut self.transport, payload)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            return Ok(());
        }
        if id != self.channels.global {
            return Ok(());
        }
        let mut p = Packet(payload);
        let header =
            share::control(&mut p).map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
        match header.kind {
            7 => {
                let (kind, payload) = self
                    .share
                    .data(header.body)
                    .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                if logon_pdu(kind, payload)? {
                    super::trace("server confirmed logon");
                    self.logon_confirmed = true;
                }
                if kind == 2 {
                    self.bitmap_update(payload, emit)
                        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                } else if kind == 27
                    && let Some(shape) = self
                        .pointers
                        .update(payload)
                        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?
                {
                    emit(Event::Pointer(shape));
                }
            }
            6 => {
                self.deactivated = true;
                self.first_picture_deadline = Some(std::time::Instant::now() + FIRST_PICTURE_WAIT);
            }
            1 => {
                self.deactivated = false;
                self.startup_picture = StartupPicture::default();
                super::trace("server reactivated the desktop");
                // Windows reactivates the share after a display layout change.
                let share = Share::from_demand(&header)
                    .map_err(|e| Ended::io(e, Failure::ProtocolFailure))?;
                share
                    .activate(&self.channels, &mut self.transport)
                    .map_err(|e| Ended::io(e, Failure::ProtocolFailure))?;
                self.share = share;
                self.pointer_position = None;
                self.pointers = Default::default();
                self.fast_fragment = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn fast(&mut self, body: &[u8], emit: &Emit) -> io::Result<()> {
        let mut p = Packet(body);
        while !p.0.is_empty() {
            let header = p.u8()?;
            if header & 0x30 != 0 && !self.share.large_pointers {
                return Err(wire::invalid("unnegotiated fast-path fragmentation"));
            }
            match header >> 6 {
                0 => {}
                2 if p.u8()? == 0 => {}
                _ => return Err(wire::invalid("unnegotiated fast-path compression")),
            }
            let len = p.le16()?;
            let data = p.take(usize::from(len))?;
            let code = header & 15;
            match (header >> 4) & 3 {
                0 if self.fast_fragment.is_none() => self.fast_update(code, data, emit)?,
                2 if self.fast_fragment.is_none() => {
                    self.fast_fragment = Some((code, data.to_vec()))
                }
                fragment @ (1 | 3) => {
                    let Some((previous, buffer)) = self.fast_fragment.as_mut() else {
                        return Err(wire::invalid("missing initial fast-path fragment"));
                    };
                    if *previous != code
                        || buffer.len() + data.len() > share::MULTIFRAGMENT_LIMIT as usize
                    {
                        return Err(wire::invalid("invalid fast-path fragment sequence"));
                    }
                    buffer.extend_from_slice(data);
                    if fragment == 1 {
                        let (_, complete) = self.fast_fragment.take().unwrap();
                        self.fast_update(code, &complete, emit)?;
                    }
                }
                _ => return Err(wire::invalid("interleaved fast-path fragments")),
            }
        }
        Ok(())
    }

    fn fast_update(&mut self, code: u8, data: &[u8], emit: &Emit) -> io::Result<()> {
        match code {
            1 => self.bitmap_update(data, emit)?,
            5 => emit(Event::Pointer(crate::PointerShape::Hidden)),
            6 => emit(Event::Pointer(crate::PointerShape::Default)),
            9 => emit(Event::Pointer(self.pointers.shape(24, &mut Packet(data))?)),
            10 => {
                if let Some(shape) = self.pointers.cached(&mut Packet(data))? {
                    emit(Event::Pointer(shape));
                }
            }
            11 => emit(Event::Pointer(
                self.pointers.new_pointer(&mut Packet(data))?,
            )),
            2 => self.bitmap_update(data, emit)?,
            3 | 8 => {}
            _ => return Err(wire::invalid("unsupported fast-path update")),
        }
        Ok(())
    }

    fn bitmap_update(&mut self, payload: &[u8], emit: &Emit) -> io::Result<()> {
        let mut p = Packet(payload);
        let kind = p.le16()?;
        // TS_UPDATE_BITMAP_DATA is updateType then numberRectangles: the palette
        // update is the one with a pad field, not this one.
        if kind == 2 {
            return self.pointers.palette(&mut p);
        }
        let count = p.le16()?;
        if kind != 1 {
            // Palette and other update types carry nothing this client renders.
            return Ok(());
        }
        for _ in 0..count {
            let bitmap = update::rect(&mut p)?;
            if bitmap.bpp != self.share.bpp() {
                return Err(wire::invalid(
                    "the server sent a bitmap depth it did not negotiate",
                ));
            }
            let pixels = update::pixels(&bitmap, self.share.width, self.share.height)?;
            self.startup_picture.observe(&pixels);
            self.first_picture_deadline = None;
            emit(Event::Frame {
                x: i32::from(bitmap.x),
                y: i32::from(bitmap.y),
                width: i32::from(bitmap.width),
                height: i32::from(bitmap.height),
                desktop_width: u32::from(self.share.width),
                desktop_height: u32::from(self.share.height),
                stride: u32::from(bitmap.width) * 4,
                pixels,
            });
        }
        Ok(())
    }

    /// Slow-path keyboard input: the same event the fast path carries, inside a
    /// Share Data PDU. Every server accepts it, which a fast-path PDU is not
    /// guaranteed to be.
    fn key(&mut self, scancode: u8, down: bool, extended: bool) -> io::Result<()> {
        self.share.input(
            &self.channels,
            &mut self.transport,
            4, // INPUT_EVENT_SCANCODE
            keyboard_flags(down, extended),
            u16::from(scancode),
            0,
        )
    }

    /// Slow-path pointer input (INPUT_EVENT_MOUSE), with the same flag encoding
    /// the fast path uses.
    fn pointer(&mut self, flags: u16, x: u16, y: u16) -> io::Result<()> {
        self.share.input(
            &self.channels,
            &mut self.transport,
            0x8001, // INPUT_EVENT_MOUSE
            flags,
            x,
            y,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activated(emit: &Emit) -> Active {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let transport =
            Transport::Plain(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let now = std::time::Instant::now();
        Active {
            transport,
            channels: Channels {
                dynamic: None,
                user: 1004,
                global: 1003,
                clipboard: None,
                drive: None,
            },
            share: Share::pending(1280, 720),
            frames: Frames::default(),
            clipboard: Clipboard::new(false, Arc::clone(emit)),
            drive: Drive::new(None, Arc::clone(emit)),
            first_picture_deadline: Some(now + FIRST_PICTURE_WAIT),
            startup_picture: StartupPicture::default(),
            deactivated: false,
            logon_deadline: now + LOGON_WAIT,
            logon_confirmed: false,
            connected: false,
            buttons: 0,
            pointer_position: None,
            pointers: Default::default(),
            fast_fragment: None,
            display: Default::default(),
        }
    }

    #[test]
    fn a_black_logon_surface_requests_one_repaint_unless_content_already_arrived() {
        let now = Instant::now();
        let mut picture = StartupPicture::default();
        assert!(!picture.redraw_due(now));
        picture.observe(&[0, 0, 0, 255]);
        assert!(!picture.redraw_due(now));
        assert!(!picture.redraw_due(now + Duration::from_millis(50)));
        assert!(picture.redraw_due(now + Duration::from_secs(1)));
        picture.observe(&[0, 0, 0, 255]);
        assert!(!picture.redraw_due(now + Duration::from_secs(2)));
        let mut picture = StartupPicture::default();
        picture.observe(&[0, 0, 0, 255]);
        assert!(!picture.redraw_due(now));
        picture.observe(&[20, 20, 20, 255]);
        assert!(!picture.redraw_due(now + Duration::from_secs(1)));
    }

    #[test]
    fn fragmented_windows_alpha_pointer_is_emitted_and_cached_after_its_last_fragment() {
        let (tx, events) = std::sync::mpsc::channel();
        let emit: Emit = Arc::new(move |event| tx.send(event).unwrap());
        let mut active = activated(&emit);
        active.share.large_pointers = true;
        let mut pdu = Vec::new();
        for word in [32u16, 1, 0, 0, 1, 2, 0, 8] {
            pdu.extend_from_slice(&word.to_le_bytes());
        }
        pdu.extend_from_slice(&[1, 3, 5, 128, 4, 6, 8, 0]);
        let mut first = vec![0x2b, 12, 0];
        first.extend_from_slice(&pdu[..12]);
        active.fast(&first, &emit).unwrap();
        assert!(events.try_recv().is_err());
        let mut last = vec![0x1b, 12, 0];
        last.extend_from_slice(&pdu[12..]);
        active.fast(&last, &emit).unwrap();
        let Event::Pointer(shape) = events.try_recv().unwrap() else {
            panic!("pointer")
        };
        let crate::PointerShape::Bitmap { pixels, .. } = &shape else {
            panic!("bitmap")
        };
        assert_eq!(&**pixels, &[4, 6, 8, 0, 1, 3, 5, 128]);
        active.fast(&[10, 2, 0, 1, 0], &emit).unwrap();
        assert_eq!(events.try_recv().unwrap(), Event::Pointer(shape));
    }

    #[test]
    fn windows_fast_path_delivers_a_headerless_bitmap_after_pointer_updates() {
        let (tx, events) = std::sync::mpsc::channel();
        let emit: Emit = Arc::new(move |event| tx.send(event).unwrap());
        let mut active = activated(&emit);
        // The short pointer PDU and the start of the next length determinant
        // can arrive in one TLS read.
        active.frames.push(&[0, 5, 6, 0, 0, 0, 0x80]);
        let Some(Incoming::Fast(pointer)) = active.frames.take().unwrap() else {
            panic!("pointer PDU")
        };
        active.fast(&pointer, &emit).unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::Pointer(crate::PointerShape::Default)
        ));
        assert!(active.first_picture_deadline.is_some());
        assert!(active.frames.take().unwrap().is_none());

        // One packet contains a synchronize update followed by one 8x4
        // uncompressed RDP6 planar bitmap (alpha, red, green, blue).
        let mut bitmap = Vec::new();
        for word in [1u16, 1, 0, 0, 7, 3, 8, 4, 32, 0x0401, 129] {
            bitmap.extend_from_slice(&word.to_le_bytes());
        }
        bitmap.push(0);
        for plane in [255, 42, 33, 24] {
            bitmap.extend_from_slice(&[plane; 32]);
        }
        let mut tail = vec![(3 + 3 + 3 + bitmap.len()) as u8, 3, 0, 0, 1];
        tail.extend_from_slice(&(bitmap.len() as u16).to_le_bytes());
        tail.extend_from_slice(&bitmap);
        active.frames.push(&tail);
        let Some(Incoming::Fast(body)) = active.frames.take().unwrap() else {
            panic!("bitmap PDU")
        };
        active.fast(&body, &emit).unwrap();
        let Event::Frame {
            width,
            height,
            pixels,
            ..
        } = events.try_recv().unwrap()
        else {
            panic!("decoded bitmap")
        };
        assert_eq!((width, height), (8, 4));
        assert_eq!(pixels, [24, 33, 42, 255].repeat(32));
        assert!(active.first_picture_deadline.is_none());
    }

    #[test]
    fn activation_without_pixels_ends_with_a_first_picture_error() {
        let emit: Emit = Arc::new(|_| {});
        let mut active = activated(&emit);
        active.first_picture_deadline = Some(std::time::Instant::now());
        let (_, commands) = crate::input::channel(None);
        let ended = active.pump(&emit, &Arc::default(), &commands).unwrap_err();
        assert_eq!(ended.failure, Failure::Io);
        assert!(ended.reason.contains("desktop picture"));
    }

    #[test]
    fn a_pdu_split_by_a_timeout_is_still_parsed() {
        // A TPKT with a 6-byte body: version 3, length 10.
        let pdu = vec![3, 0, 0, 10, b'a', b'b', b'c', b'd', b'e', b'f'];
        let mut frames = Frames::default();
        frames.push(&pdu[..5]);
        assert!(frames.take().unwrap().is_none(), "half a PDU is not a PDU");
        frames.push(&pdu[5..]);
        match frames.take().unwrap() {
            Some(Incoming::Slow(body)) => assert_eq!(body, b"abcdef"),
            other => panic!("expected the body, got {}", other.is_some()),
        }
        assert!(frames.take().unwrap().is_none(), "nothing left");
    }

    #[test]
    fn two_pdus_in_one_read_arrive_one_at_a_time() {
        let one = vec![3, 0, 0, 6, 1, 2];
        let two = vec![3, 0, 0, 5, 9];
        let mut frames = Frames::default();
        frames.push(&one);
        frames.push(&two);
        match frames.take().unwrap() {
            Some(Incoming::Slow(body)) => assert_eq!(body, [1, 2]),
            _ => panic!("first PDU"),
        }
        match frames.take().unwrap() {
            Some(Incoming::Slow(body)) => assert_eq!(body, [9]),
            _ => panic!("second PDU"),
        }
        assert!(frames.take().unwrap().is_none());
    }

    #[test]
    fn a_timeout_in_the_middle_of_a_pdu_keeps_what_arrived() {
        // A peer sends the first five bytes of a PDU, waits past the read timeout,
        // then sends the rest. The read really times out, and the second call still
        // parses the whole PDU because the first five bytes were kept.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("peer accepted");
            stream.write_all(&[3, 0, 0, 10, b'a']).expect("first half");
            std::thread::sleep(Duration::from_millis(250));
            stream.write_all(b"bcdef").expect("second half");
        });
        let mut stream = std::net::TcpStream::connect(address).expect("client");
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("timeout");
        let mut frames = Frames::default();
        let timed_out = match next_frame(&mut stream, &mut frames) {
            Err(error) => matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            Ok(_) => false,
        };
        assert!(
            timed_out,
            "the read has to time out in the middle of the PDU"
        );
        // The loop the session runs: every timeout leaves the bytes in place and the
        // next attempt continues, until the rest of the same PDU arrives.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let incoming = loop {
            match next_frame(&mut stream, &mut frames) {
                Ok(incoming) => break incoming,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) && std::time::Instant::now() < deadline => {}
                Err(error) => panic!("the rest of the PDU: {error}"),
            }
        };
        match incoming {
            Incoming::Slow(body) => assert_eq!(body, b"abcdef"),
            Incoming::Fast(_) => panic!("the PDU was slow-path"),
        }
        peer.join().expect("peer thread");
    }

    /// The keyboard event one press or release puts on the wire: its flags and the
    /// scancode, read back out of the Share Data PDU a server would receive.
    fn keyboard_pdu(down: bool, scancode: u8, extended: bool) -> (u16, u16) {
        let channels = Channels {
            dynamic: None,
            user: 1003,
            global: 1007,
            clipboard: None,
            drive: None,
        };
        let share = Share::pending(800, 600);
        let mut out = Vec::new();
        share
            .input(
                &channels,
                &mut out,
                4, // INPUT_EVENT_SCANCODE
                keyboard_flags(down, extended),
                u16::from(scancode),
                0,
            )
            .expect("the input PDU is written");

        // TPKT, then MCS SendDataIndication: type, sending user, channel, flags,
        // length, body.
        let mut mcs = Packet(&out);
        assert_eq!(mcs.u8().expect("TPKT version"), 3, "TPKT wraps the PDU");
        mcs.u8().expect("TPKT flags");
        let tpkt_len = usize::from(mcs.be16().expect("TPKT length"));
        assert_eq!(tpkt_len, out.len(), "the TPKT says the whole PDU");
        assert_eq!(
            mcs.take(3).expect("X.224 data"),
            [2, 0xf0, 0x80],
            "X.224 wraps the MCS PDU"
        );
        assert_eq!(mcs.u8().expect("type"), 0x64, "MCS send-data");
        mcs.take(2).expect("sending user");
        mcs.be16().expect("channel");
        assert_eq!(mcs.u8().expect("flags") & 0x30, 0x30, "one whole body");
        let len = mcs.per_length().expect("length");
        let body = mcs.take(len).expect("body");

        // Share Control header, then the Share Data header, then the events.
        let mut control = Packet(body);
        control.le16().expect("total length");
        assert_eq!(
            control.le16().expect("pdu type") & 0x0f,
            7,
            "PDUTYPE_DATAPDU"
        );
        control.le16().expect("source");
        let rest = control.take(control.0.len()).expect("share data");
        let mut data = Packet(rest);
        data.le32().expect("share id");
        data.take(2).expect("padding and stream id");
        let data_len = data.le16().expect("data length");
        assert_eq!(data.u8().expect("data type"), 28, "PDUTYPE2_INPUT");
        assert_eq!(data.u8().expect("compression"), 0, "no bulk compression");
        data.take(2).expect("padding");
        let events = data
            .take(usize::from(data_len).saturating_sub(4))
            .expect("input events");
        let mut event = Packet(events);
        assert_eq!(event.le16().expect("event count"), 1, "one event");
        event.le16().expect("padding");
        event.le32().expect("event time");
        assert_eq!(
            event.le16().expect("message type"),
            4,
            "INPUT_EVENT_SCANCODE"
        );
        (
            event.le16().expect("keyboard flags"),
            event.le16().expect("key code"),
        )
    }

    #[test]
    fn a_slow_path_key_release_is_flagged_as_a_release() {
        // The slow path's bits, not the fast path's: a release marked 0x01 reads as
        // a press, so every typed character arrived twice.
        assert_eq!(keyboard_flags(true, false), 0, "a plain press has no flags");
        assert_eq!(
            keyboard_flags(false, false),
            0x8000,
            "a release is KBD_FLAGS_RELEASE"
        );
        assert_eq!(
            keyboard_flags(true, true),
            0x0100,
            "an extended key is KBD_FLAGS_EXTENDED"
        );
        assert_eq!(
            keyboard_flags(false, true),
            0x8100,
            "an extended release carries both"
        );

        // And in the PDU a server really receives.
        let (down_flags, down_code) = keyboard_pdu(true, 0x1e, false);
        let (up_flags, up_code) = keyboard_pdu(false, 0x1e, false);
        assert_eq!(down_flags & 0x8000, 0, "the press is not a release");
        assert_eq!(up_flags & 0x8000, 0x8000, "the release is a release");
        assert_eq!(
            down_flags ^ up_flags,
            0x8000,
            "the pair differs only by the release flag"
        );
        assert_eq!(
            (down_code, up_code),
            (0x1e, 0x1e),
            "the same key goes down and up"
        );
        let (extended_flags, _) = keyboard_pdu(true, 0x48, true);
        assert_eq!(
            extended_flags & 0x0100,
            0x0100,
            "an up-arrow press carries the extended flag"
        );
        assert_eq!(extended_flags & 0x8000, 0, "and it is still a press");
    }

    #[test]
    fn a_logon_confirmation_maps_to_its_outcome() {
        assert!(logon_pdu(38, &[1, 0, 0, 0]).unwrap());
        assert!(!logon_pdu(47, &[0; 4]).unwrap());
        assert!(!logon_pdu(2, &[]).unwrap());
        assert_eq!(
            logon_pdu(47, &[0, 0]).unwrap_err().failure,
            Failure::ProtocolFailure
        );
    }

    #[test]
    fn bad_capabilities_and_authentication_have_distinct_failure_reasons() {
        let rejected = logon_pdu(47, &4330u32.to_le_bytes()).unwrap_err();
        assert_eq!(rejected.failure, Failure::ProtocolFailure);
        assert!(rejected.reason.contains("capabilities"));
        assert!(rejected.reason.contains("0x000010EA"));
        for (code, failure) in [
            (9u32, Failure::Authentication),
            (10, Failure::Authentication),
            (5, Failure::RemoteClosed),
            (0x10d4, Failure::ProtocolFailure),
        ] {
            assert_eq!(
                logon_pdu(47, &code.to_le_bytes()).unwrap_err().failure,
                failure
            );
        }
    }
}
