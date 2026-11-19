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
use crate::{Event, Failure, MAX_FRAME_BYTES, State};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// How long the handshake may take before the attempt is abandoned.
const HANDSHAKE: Duration = Duration::from_secs(20);
/// A dropped session is retried once before it is reported as failed.
const ATTEMPTS: u8 = 2;

/// Ends one attempt with the business failure it maps to.
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
            io::ErrorKind::Unsupported => Failure::UnsupportedSecurity,
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset => Failure::RemoteClosed,
            _ if during != Failure::None => during,
            _ => Failure::Io,
        };
        Ended::new(failure, error.to_string())
    }
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
    Fast,
}

/// Runs one attempt, retrying once if the server drops an established session.
pub(super) fn run(
    config: &Config,
    emit: &Emit,
    control: &Arc<Control>,
    commands: Receiver<Command>,
) {
    for attempt in 1..=ATTEMPTS {
        if control.cancelled() {
            return;
        }
        match connect(config, emit, control, &commands) {
            Ok(mut active) => {
                publish(emit, control, &State::Connected, Failure::None, "");
                match active.pump(emit, control, &commands) {
                    Ok(()) => {
                        publish(emit, control, &State::Closed, Failure::None, "");
                        return;
                    }
                    Err(ended) => {
                        if control.cancelled() {
                            publish(emit, control, &State::Closed, Failure::None, "");
                            return;
                        }
                        if attempt < ATTEMPTS && ended.failure == Failure::RemoteClosed {
                            publish(
                                emit,
                                control,
                                &State::Connecting,
                                Failure::None,
                                "the session dropped; reconnecting",
                            );
                            continue;
                        }
                        publish(
                            emit,
                            control,
                            &State::Failed(ended.failure),
                            ended.failure,
                            &ended.reason,
                        );
                        return;
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
                return;
            }
        }
    }
}

struct Active {
    transport: Transport,
    channels: Channels,
    share: Share,
    frame: Vec<u8>,
    stride: usize,
    width: u16,
    height: u16,
    clipboard: Clipboard,
    drive: Drive,
    /// The button state the host last reported, so each edge carries its button.
    buttons: u32,
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
    let mut secure = security::tls(transport, &config.host, verify).map_err(|error| {
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
        frame: vec![0; usize::from(config.width) * usize::from(config.height) * 4],
        stride: usize::from(config.width) * 4,
        width: config.width,
        height: config.height,
        clipboard: Clipboard::new(config.clipboard, Arc::clone(emit)),
        drive: Drive::new(config.drive.clone(), Arc::clone(emit)),
        buttons: 0,
    };
    active.await_activation(control)?;
    // A synchronize event is how a client reports the keyboard toggle state;
    // servers expect it before forwarding input.
    active
        .share
        .input(&active.channels, &mut active.transport, 0, 0, 0, 0)
        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
    super::trace("desktop activated");
    let _ = commands;
    Ok(active)
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
            let width = share.width;
            let height = share.height;
            if width != self.width || height != self.height {
                self.width = width;
                self.height = height;
                self.stride = usize::from(width) * 4;
                self.frame = vec![0; self.stride * usize::from(height)];
            }
            share
                .activate(&self.channels, &mut self.transport)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            self.share = share;
            self.clipboard
                .announce(&self.channels, &mut self.transport)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            return Ok(());
        }
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
            self.drain(emit, commands)?;
            self.clipboard
                .poll(&self.channels, &mut self.transport)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            match self.read() {
                Ok(Incoming::Slow(body)) => self.slow(&body, emit)?,
                Ok(Incoming::Fast) => {
                    return Err(Ended::new(
                        Failure::ProtocolFailure,
                        "the server sent fast-path output the client did not offer",
                    ));
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
    fn drain(&mut self, emit: &Emit, commands: &Receiver<Command>) -> Result<(), Ended> {
        loop {
            match commands.try_recv() {
                Ok(Command::Key {
                    scancode,
                    down,
                    extended,
                }) => self
                    .key(scancode, down, extended)
                    .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?,
                Ok(Command::Pointer { buttons, x, y }) => {
                    let flags = super::pointer_flags(self.buttons, buttons);
                    self.buttons = buttons;
                    self.pointer(flags, x, y)
                        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                }
                Ok(Command::Clipboard(entries)) => self
                    .clipboard
                    .offer(&self.channels, &mut self.transport, &entries)
                    .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    let _ = emit;
                    return Ok(());
                }
            }
        }
    }

    fn read(&mut self) -> io::Result<Incoming> {
        let mut first = [0u8; 1];
        self.transport.read_exact(&mut first)?;
        if first[0] == 3 {
            let mut header = [0u8; 3];
            self.transport.read_exact(&mut header)?;
            if header[0] != 0 {
                return Err(wire::invalid("invalid TPKT version"));
            }
            let length = usize::from(u16::from_be_bytes([header[1], header[2]]));
            let mut body = vec![
                0;
                length
                    .checked_sub(4)
                    .ok_or_else(|| wire::invalid("short TPKT"))?
            ];
            self.transport.read_exact(&mut body)?;
            Ok(Incoming::Slow(body))
        } else {
            // Fast-path: header byte, 16-bit length, then the updates. Nothing is
            // parsed because the client never advertises that output path.
            let mut length = [0u8; 2];
            self.transport.read_exact(&mut length)?;
            let total = usize::from(u16::from_be_bytes(length));
            let mut body = vec![
                0;
                total
                    .checked_sub(3)
                    .ok_or_else(|| wire::invalid("short fast-path PDU"))?
            ];
            self.transport.read_exact(&mut body)?;
            Ok(Incoming::Fast)
        }
    }

    fn read_slow(&mut self) -> Result<Option<Vec<u8>>, Ended> {
        match self.read() {
            Ok(Incoming::Slow(body)) => Ok(Some(body)),
            Ok(Incoming::Fast) => Ok(None),
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
        if Some(id) == self.channels.clipboard {
            self.clipboard
                .on_data(&self.channels, &mut self.transport, payload)
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
                if kind == 2 {
                    self.bitmap_update(payload, emit)?;
                }
            }
            2 => {
                return Err(Ended::new(
                    Failure::RemoteClosed,
                    "the server deactivated the desktop",
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn bitmap_update(&mut self, payload: &[u8], emit: &Emit) -> Result<(), Ended> {
        let mut p = Packet(payload);
        let kind = p
            .le16()
            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
        // TS_UPDATE_BITMAP_DATA is updateType then numberRectangles: the palette
        // update is the one with a pad field, not this one.
        let count = p
            .le16()
            .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
        if kind != 1 {
            // Palette and other update types carry nothing this client renders.
            return Ok(());
        }
        for _ in 0..count {
            let bitmap =
                update::rect(&mut p).map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            if bitmap.bpp != self.share.bpp() {
                return Err(Ended::new(
                    Failure::ProtocolFailure,
                    "the server sent a bitmap depth it did not negotiate",
                ));
            }
            update::draw(&mut self.frame, self.stride, self.height, &bitmap)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            let pixels = update::region(&self.frame, self.stride, &bitmap);
            if pixels.len() > MAX_FRAME_BYTES {
                return Err(Ended::new(
                    Failure::Limit,
                    "the frame exceeds the accepted size",
                ));
            }
            emit(Event::Frame {
                x: i32::from(bitmap.x),
                y: i32::from(bitmap.y),
                width: i32::from(bitmap.width),
                height: i32::from(bitmap.height),
                desktop_width: u32::from(self.width),
                desktop_height: u32::from(self.height),
                stride: u32::try_from(usize::from(bitmap.width) * 4).unwrap_or(0),
                pixels,
            });
        }
        Ok(())
    }

    /// Slow-path keyboard input: the same event the fast path carries, inside a
    /// Share Data PDU. Every server accepts it, which a fast-path PDU is not
    /// guaranteed to be.
    fn key(&mut self, scancode: u8, down: bool, extended: bool) -> io::Result<()> {
        let mut flags = 0u16;
        if !down {
            flags |= 0x01; // KBDFLAGS_RELEASE
        }
        if extended {
            flags |= 0x02; // KBDFLAGS_EXTENDED
        }
        self.share.input(
            &self.channels,
            &mut self.transport,
            4, // INPUT_EVENT_SCANCODE
            flags,
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
