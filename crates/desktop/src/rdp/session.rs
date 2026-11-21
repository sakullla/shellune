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
use crate::{Event, Failure, MAX_FRAME_BYTES, State};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How long the handshake may take before the attempt is abandoned.
const HANDSHAKE: Duration = Duration::from_secs(20);
/// A PDU arrives as a byte stream. A read that times out in the middle of one must
/// not lose what was already read, so bytes are kept here until a whole TPKT is
/// buffered; only then is it handed out.
#[derive(Default)]
struct Frames {
    pending: Vec<u8>,
}

impl Frames {
    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// The next complete PDU body, when one is buffered.
    fn take(&mut self) -> io::Result<Option<Incoming>> {
        if self.pending.len() < 4 {
            return Ok(None);
        }
        let slow = self.pending[0] == 3;
        let (header, total) = if slow {
            if self.pending[1] != 0 {
                return Err(wire::invalid("invalid TPKT version"));
            }
            let length = usize::from(u16::from_be_bytes([self.pending[2], self.pending[3]]));
            (4usize, length)
        } else {
            // Fast-path: one header byte, a 16-bit length, then the updates.
            let length = usize::from(u16::from_be_bytes([self.pending[1], self.pending[2]]));
            (3usize, length)
        };
        if total < header {
            return Err(wire::invalid("short RDP pdu"));
        }
        if self.pending.len() < total {
            return Ok(None);
        }
        let rest = self.pending.split_off(total);
        let frame = std::mem::replace(&mut self.pending, rest);
        let body = frame[header..].to_vec();
        Ok(Some(if slow {
            Incoming::Slow(body)
        } else {
            Incoming::Fast
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
/// One read wait once the desktop is up: the loop returns to its cancellation and
/// deadline checks after each, and a partial record stays buffered.
const POLL: Duration = Duration::from_millis(32);

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
    Fast,
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

#[cfg(test)]
mod tests {
    use super::*;

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
            Incoming::Fast => panic!("the PDU was slow-path"),
        }
        peer.join().expect("peer thread");
    }

    /// The keyboard event one press or release puts on the wire: its flags and the
    /// scancode, read back out of the Share Data PDU a server would receive.
    fn keyboard_pdu(down: bool, scancode: u8, extended: bool) -> (u16, u16) {
        let channels = Channels {
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
        // PDUTYPE2_SAVE_SESSION_INFO confirms the logon.
        assert_eq!(logon_pdu(38, &[1, 0, 0, 0]), Some(Logon::Confirmed));
        // PDUTYPE2_SET_ERROR_INFO with zero is a confirmation ...
        assert_eq!(logon_pdu(47, &[0, 0, 0, 0]), Some(Logon::Confirmed));
        // ... and with a code is a refusal that keeps the code.
        assert_eq!(logon_pdu(47, &5u32.to_le_bytes()), Some(Logon::Refused(5)));
        // Anything else is not a logon result.
        assert_eq!(logon_pdu(2, &[0, 0, 0, 0]), None);
        assert_eq!(logon_pdu(47, &[0, 0]), None);
    }
}

/// What the server said about the logon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Logon {
    /// The server confirmed the logon.
    Confirmed,
    /// The server refused it, with the error code it gave.
    Refused(u32),
}

struct Active {
    transport: Transport,
    channels: Channels,
    share: Share,
    /// Set when the server says the logon succeeded or failed.
    logon: Option<Logon>,
    /// Bytes of a PDU that has not fully arrived yet.
    frames: Frames,
    /// Whether Connected has been published.
    connected: bool,
    /// When an unconfirmed logon stops being waited for.
    logon_deadline: std::time::Instant,
    frame: Vec<u8>,
    stride: usize,
    width: u16,
    height: u16,
    clipboard: Clipboard,
    drive: Drive,
    /// The button state the host last reported, so each edge carries its button.
    buttons: u32,
    pointers: super::pointer::Pointers,
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
        pointers: Default::default(),
        logon: None,
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
    active.set_poll_timeout(POLL);
    let _ = commands;
    Ok(active)
}

/// The logon outcome a share data PDU carries: PDUTYPE2_SAVE_SESSION_INFO
/// confirms it, PDUTYPE2_SET_ERROR_INFO reports a code — zero is success and
/// anything else is a refusal.
fn logon_pdu(kind: u8, payload: &[u8]) -> Option<Logon> {
    match kind {
        38 => Some(Logon::Confirmed),
        47 => {
            let bytes = payload.get(..4)?;
            let code = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            Some(if code == 0 {
                Logon::Confirmed
            } else {
                Logon::Refused(code)
            })
        }
        _ => None,
    }
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

    /// Publishes Connected once, and only when the logon is settled: confirmed by
    /// the server, or unconfirmed after the wait because the server reports logon
    /// results only on its own screen (xrdp without CredSSP). A refusal is an
    /// authentication failure and never a session.
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

    fn announce_logon(&mut self, emit: &Emit, control: &Control) -> Result<(), Ended> {
        if self.connected {
            return Ok(());
        }
        match self.logon {
            Some(Logon::Refused(code)) => Err(Ended::new(
                Failure::Authentication,
                format!("the server refused the logon (code {code})"),
            )),
            Some(Logon::Confirmed) => {
                publish(emit, control, &State::Connected, Failure::None, "");
                self.connected = true;
                Ok(())
            }
            None if std::time::Instant::now() > self.logon_deadline => {
                publish(
                    emit,
                    control,
                    &State::Connected,
                    Failure::None,
                    "logon not confirmed: the server authenticates through its own screen",
                );
                self.connected = true;
                Ok(())
            }
            None => Ok(()),
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
            self.announce_logon(emit, control)?;
            self.drain(commands)?;
            self.clipboard
                .poll(&self.channels, &mut self.transport)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            match read_chunk(&mut self.transport, &mut self.frames) {
                Ok(None) => continue,
                Ok(Some(Incoming::Slow(body))) => {
                    self.slow(&body, emit)?;
                    if let Some(Logon::Refused(code)) = self.logon {
                        return Err(Ended::new(
                            Failure::Authentication,
                            format!("the server refused the logon (code {code})"),
                        ));
                    }
                }
                Ok(Some(Incoming::Fast)) => {
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
                    self.buttons = buttons;
                    self.pointer(flags, x, y)
                        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
                }
                Some(Command::Clipboard(entries)) => self
                    .clipboard
                    .offer(&self.channels, &mut self.transport, &entries)
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
                if let Some(logon) = logon_pdu(kind, payload) {
                    super::trace(&format!("logon pdu kind={kind} {logon:?}"));
                    self.logon = Some(logon);
                }
                if kind == 2 {
                    self.bitmap_update(payload, emit)?;
                } else if kind == 27
                    && let Some(shape) = self
                        .pointers
                        .update(payload)
                        .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?
                {
                    emit(Event::Pointer(shape));
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
            let used = update::draw(&mut self.frame, self.stride, self.height, &bitmap)
                .map_err(|error| Ended::io(error, Failure::ProtocolFailure))?;
            // Advance by what the stream used, not by the length the server
            // declared: one of them counts a header pad the other does not write.
            p.take(used)
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
