//! The native desktop boundary (R18-R20, R23).
//!
//! Everything here runs the real shim against the real libraries built from
//! `vendor/`: LibVNCClient over real sockets, FreeRDP over real sockets. There is
//! no RDP or VNC server on this machine, so the peer is a scripted TCP server that
//! speaks exactly the bytes the test needs and nothing more. That is enough to
//! prove the parts this task owns - the ABI, cancellation, close ordering, the
//! failure classification and a first frame with input - and it is *not* evidence
//! that any particular real server works. The endpoint matrix belongs to T5 and is
//! reported as unverified until it actually runs.
//!
//! When the native libraries were not built, every test here reports SKIP and says
//! which command produces them. Nothing is faked into a pass. With
//! `SHELLUNE_REQUIRE_NATIVE=1` (set by CI, which builds them first) a missing backend
//! fails instead, so a broken native build cannot pass as a row of skips.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use shellune_desktop::probe::{self, DriveRequest};
use shellune_desktop::{
    ClipboardEntry, ClipboardKind, DesktopClient, DriveProblem, DriveRefusal, Error, Event,
    Failure, Notice, PointerButtons, Protocol, State, VncSecurity, backend_info, callback_panics,
    leaked_handles,
};

/// How long a scripted peer or a worker may take before the test calls it a hang.
const PATIENCE: Duration = Duration::from_secs(10);

fn backends_built() -> bool {
    let mut built = true;
    for protocol in [Protocol::Rdp, Protocol::Vnc] {
        match backend_info(protocol) {
            Ok(info) => eprintln!("backend {protocol:?}: {info}"),
            Err(Error::Unavailable(reason)) => {
                if std::env::var_os("SHELLUNE_REQUIRE_NATIVE").is_some_and(|value| value == "1") {
                    panic!(
                        "{protocol:?} is not built ({reason}), and SHELLUNE_REQUIRE_NATIVE=1 \
                         says it must be"
                    );
                }
                eprintln!(
                    "SKIP: {protocol:?} is not built here ({reason}); run                      packaging/native/build_native.py"
                );
                built = false;
            }
            Err(error) => panic!("backend info failed: {error}"),
        }
    }
    built
}

/// A port nothing is listening on.
///
/// Port 1 (tcpmux) is unused on every machine this runs on and lies below the
/// ephemeral range, so no scripted peer of a test running in parallel can be handed
/// it. A port taken from the ephemeral range and released again can be: a Windows CI
/// runner gave one to a silent peer, and the "closed" port then accepted.
fn a_closed_port() -> u16 {
    const CLOSED: u16 = 1;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], CLOSED));
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).is_err(),
        "something listens on 127.0.0.1:{CLOSED}; these tests need it closed"
    );
    CLOSED
}

fn pump(
    client: &DesktopClient,
    timeout: Duration,
    mut done: impl FnMut(&Event) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match client.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                let stop = done(&event);
                seen.push(event);
                if stop {
                    break;
                }
            }
            Err(_) => continue,
        }
    }
    seen
}

fn states(events: &[Event]) -> Vec<State> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::State { state, .. } => Some(*state),
            _ => None,
        })
        .collect()
}

fn never_connected(events: &[Event]) {
    assert!(
        !states(events).contains(&State::Connected),
        "the session must not report Connected: {events:?}"
    );
}

// ---------------------------------------------------------------- scripted peer

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClientMessage {
    PixelFormat,
    Encodings(Vec<i32>),
    UpdateRequest {
        incremental: bool,
    },
    Key {
        keysym: u32,
        down: bool,
    },
    Pointer {
        x: u16,
        y: u16,
        mask: u8,
    },
    /// Classic RFB cut text: Latin-1 bytes.
    CutText(Vec<u8>),
    /// Extended clipboard: the action/format flags and, for Provide, the text.
    ExtendedCutText {
        flags: u32,
        text: Option<String>,
    },
    Other(u8),
}

/// The ExtendedClipboard pseudo-encoding, as the client lists it.
const EXTENDED_CLIPBOARD: i32 = 0xC0A1_E5CEu32 as i32;
const EXT_TEXT: u32 = 1;
const EXT_CAPS: u32 = 1 << 24;
const EXT_REQUEST: u32 = 1 << 25;
const EXT_PEEK: u32 = 1 << 26;
const EXT_NOTIFY: u32 = 1 << 27;
const EXT_PROVIDE: u32 = 1 << 28;

/// What the scripted peer puts on its clipboard.
#[derive(Clone, Copy)]
enum Cut {
    /// Sent as classic cut text right after ServerInit.
    Latin1(&'static [u8]),
    /// Sent as an extended clipboard Provide once the client asked for the
    /// extension (and the peer offers it).
    Utf8(&'static str),
}

/// What the scripted peer does once a socket is accepted.
#[derive(Clone)]
enum Script {
    /// Accept and say nothing: the client must be able to cancel.
    Silent,
    /// Announce only these security types, then wait for the client's choice.
    SecurityTypes(Vec<u8>),
    /// Complete a 3.8 handshake and serve one raw frame, then keep reading input.
    /// With `extended`, the peer answers a client that lists the ExtendedClipboard
    /// encoding with its capabilities, like a server that supports it.
    Session {
        width: u16,
        height: u16,
        pixel: [u8; 4],
        clipboard: Option<Cut>,
        extended: bool,
    },
}

struct Scripted {
    port: u16,
    messages: Arc<Mutex<Vec<ClientMessage>>>,
    accepted: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Scripted {
    fn start(script: Script) -> Scripted {
        Self::start_after(script, Duration::ZERO)
    }

    fn start_after(script: Script, delay: Duration) -> Scripted {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the scripted peer");
        let port = listener.local_addr().expect("peer address").port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicBool::new(false));

        let log = Arc::clone(&messages);
        let saw = Arc::clone(&accepted);
        let thread = thread::spawn(move || {
            // Like a real server, accept as many connections as the client opens: a
            // refused handshake may be followed by the shim's security-type probe.
            while let Ok((stream, _)) = listener.accept() {
                saw.store(true, Ordering::SeqCst);
                thread::sleep(delay);
                let _ = stream.set_nodelay(true);
                match script.clone() {
                    Script::Silent => {
                        // Hold the connection open and answer nothing until the client
                        // gives up or the test drops the listener.
                        let mut sink = stream;
                        let mut buffer = [0u8; 64];
                        while matches!(sink.read(&mut buffer), Ok(size) if size > 0) {}
                    }
                    Script::SecurityTypes(types) => {
                        let mut stream = stream;
                        let _ = stream.set_read_timeout(Some(PATIENCE));
                        let _ = stream.write_all(b"RFB 003.008\n");
                        let mut version = [0u8; 12];
                        if stream.read_exact(&mut version).is_err() {
                            continue;
                        }
                        let mut announcement = vec![types.len() as u8];
                        announcement.extend_from_slice(&types);
                        let _ = stream.write_all(&announcement);
                        let mut choice = [0u8; 1];
                        let _ = stream.read_exact(&mut choice);
                    }
                    Script::Session {
                        width,
                        height,
                        pixel,
                        clipboard,
                        extended,
                    } => {
                        // One session per test: after it ends there is nothing left to
                        // serve, and a second accept would only hide client bugs.
                        let peer = Peer {
                            width,
                            height,
                            pixel,
                            clipboard,
                            extended,
                        };
                        serve_session(stream, peer, Arc::clone(&log));
                        return;
                    }
                }
            }
        });

        Scripted {
            port,
            messages,
            accepted,
            thread: Some(thread),
        }
    }

    fn messages(&self) -> Vec<ClientMessage> {
        self.messages.lock().expect("message log").clone()
    }

    fn wait_for(&self, wanted: ClientMessage) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self.messages().contains(&wanted) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        // The peer thread owns its socket and exits on its own read timeout, so the
        // test detaches it rather than blocking on a server it just cancelled.
        let _ = self.thread.take();
    }
}

fn read_message(stream: &mut TcpStream, message_type: u8) -> Option<ClientMessage> {
    match message_type {
        0 => {
            let mut padding = [0u8; 19];
            stream.read_exact(&mut padding).ok()?;
            Some(ClientMessage::PixelFormat)
        }
        2 => {
            let mut header = [0u8; 3];
            stream.read_exact(&mut header).ok()?;
            let count = u16::from_be_bytes([header[1], header[2]]) as usize;
            if count > 256 {
                return None;
            }
            let mut raw = vec![0u8; count * 4];
            stream.read_exact(&mut raw).ok()?;
            let encodings = raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| i32::from_be_bytes(*chunk))
                .collect();
            Some(ClientMessage::Encodings(encodings))
        }
        3 => {
            let mut rest = [0u8; 9];
            stream.read_exact(&mut rest).ok()?;
            Some(ClientMessage::UpdateRequest {
                incremental: rest[0] != 0,
            })
        }
        4 => {
            let mut rest = [0u8; 7];
            stream.read_exact(&mut rest).ok()?;
            Some(ClientMessage::Key {
                down: rest[0] != 0,
                keysym: u32::from_be_bytes([rest[3], rest[4], rest[5], rest[6]]),
            })
        }
        5 => {
            let mut rest = [0u8; 5];
            stream.read_exact(&mut rest).ok()?;
            Some(ClientMessage::Pointer {
                mask: rest[0],
                x: u16::from_be_bytes([rest[1], rest[2]]),
                y: u16::from_be_bytes([rest[3], rest[4]]),
            })
        }
        6 => {
            let mut header = [0u8; 7];
            stream.read_exact(&mut header).ok()?;
            let length = i32::from_be_bytes([header[3], header[4], header[5], header[6]]);
            let size = length.unsigned_abs() as usize;
            if size > 1 << 21 {
                return None;
            }
            let mut payload = vec![0u8; size];
            stream.read_exact(&mut payload).ok()?;
            if length >= 0 {
                return Some(ClientMessage::CutText(payload));
            }
            // Extended clipboard: flags, then for Provide a zlib stream holding a
            // big-endian size and the NUL-terminated text.
            let flags = u32::from_be_bytes(payload.get(..4)?.try_into().ok()?);
            let mut text = None;
            if flags & EXT_PROVIDE != 0 {
                let plain = zlib_inflate(&payload[4..])?;
                let declared = u32::from_be_bytes(plain.get(..4)?.try_into().ok()?) as usize;
                let mut body = plain.get(4..4 + declared)?.to_vec();
                while body.last() == Some(&0) {
                    body.pop();
                }
                text = Some(String::from_utf8(body).ok()?);
            }
            Some(ClientMessage::ExtendedCutText { flags, text })
        }
        other => Some(ClientMessage::Other(other)),
    }
}

struct Peer {
    width: u16,
    height: u16,
    pixel: [u8; 4],
    clipboard: Option<Cut>,
    extended: bool,
}

/// A server cut text message with a negative length: the extended clipboard form.
fn extended_message(flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = flags.to_be_bytes().to_vec();
    payload.extend_from_slice(body);
    let mut message = vec![3u8, 0, 0, 0];
    message.extend_from_slice(&(-(payload.len() as i32)).to_be_bytes());
    message.extend_from_slice(&payload);
    message
}

fn serve_session(mut stream: TcpStream, peer: Peer, log: Arc<Mutex<Vec<ClientMessage>>>) {
    let Peer {
        width,
        height,
        pixel,
        clipboard,
        extended,
    } = peer;
    let _ = stream.set_read_timeout(Some(PATIENCE));
    if stream.write_all(b"RFB 003.008\n").is_err() {
        return;
    }
    let mut version = [0u8; 12];
    if stream.read_exact(&mut version).is_err() {
        return;
    }
    // Security: one type, None.
    if stream.write_all(&[1u8, 1u8]).is_err() {
        return;
    }
    let mut choice = [0u8; 1];
    if stream.read_exact(&mut choice).is_err() {
        return;
    }
    // SecurityResult 0 = OK, then ClientInit.
    if stream.write_all(&[0u8, 0u8, 0u8, 0u8]).is_err() {
        return;
    }
    let mut client_init = [0u8; 1];
    if stream.read_exact(&mut client_init).is_err() {
        return;
    }

    let mut server_init = Vec::new();
    server_init.extend_from_slice(&width.to_be_bytes());
    server_init.extend_from_slice(&height.to_be_bytes());
    // Pixel format: 32bpp, depth 24, little endian, true colour, shifts 16/8/0.
    server_init.extend_from_slice(&[32, 24, 0, 1]);
    for value in [255u16, 255, 255] {
        server_init.extend_from_slice(&value.to_be_bytes());
    }
    for shift in [16u8, 8, 0] {
        server_init.push(shift);
    }
    server_init.extend_from_slice(&[0, 0, 0]);
    let name = b"shellune-scripted";
    server_init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    server_init.extend_from_slice(name);
    if stream.write_all(&server_init).is_err() {
        return;
    }

    if let Some(Cut::Latin1(bytes)) = clipboard {
        let mut cut = vec![3u8, 0, 0, 0];
        cut.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        cut.extend_from_slice(bytes);
        let _ = stream.write_all(&cut);
    }

    loop {
        let mut header = [0u8; 1];
        match stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(_) => return,
        }
        let Some(message) = read_message(&mut stream, header[0]) else {
            return;
        };
        let served_frame = matches!(message, ClientMessage::UpdateRequest { incremental: false });
        let wants_extended = extended
            && matches!(&message, ClientMessage::Encodings(list) if list.contains(&EXTENDED_CLIPBOARD));
        log.lock().expect("message log").push(message);
        if wants_extended {
            // Capabilities: text, every action, and the largest text accepted.
            let caps = extended_message(
                EXT_CAPS | EXT_TEXT | EXT_REQUEST | EXT_PEEK | EXT_NOTIFY | EXT_PROVIDE,
                &(1u32 << 20).to_be_bytes(),
            );
            if stream.write_all(&caps).is_err() {
                return;
            }
            if let Some(Cut::Utf8(text)) = clipboard {
                let mut plain = ((text.len() + 1) as u32).to_be_bytes().to_vec();
                plain.extend_from_slice(text.as_bytes());
                plain.push(0);
                let provide = extended_message(EXT_PROVIDE | EXT_TEXT, &zlib_stored(&plain));
                if stream.write_all(&provide).is_err() {
                    return;
                }
            }
        }
        if served_frame {
            let mut update = vec![0u8, 0];
            update.extend_from_slice(&1u16.to_be_bytes());
            for value in [0u16, 0] {
                update.extend_from_slice(&value.to_be_bytes());
            }
            for value in [width, height] {
                update.extend_from_slice(&value.to_be_bytes());
            }
            update.extend_from_slice(&0i32.to_be_bytes()); // Raw encoding
            for _ in 0..(width as usize * height as usize) {
                update.extend_from_slice(&pixel);
            }
            if stream.write_all(&update).is_err() {
                return;
            }
        }
    }
}

// ------------------------------------------------------------------- zlib

/// A zlib stream holding `data` in one stored block, which every inflater reads.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    assert!(data.len() <= 0xFFFF, "one stored block");
    let mut out = vec![0x78, 0x01, 0x01];
    let len = data.len() as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u32,
    count: u32,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> Option<u32> {
        while self.count < n {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buffer |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let value = self.buffer & ((1u32 << n) - 1);
        self.buffer = self.buffer.checked_shr(n).unwrap_or(0);
        self.count -= n;
        Some(value)
    }
}

struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Huffman {
        let mut count = [0u16; 16];
        for &len in lengths {
            count[len as usize] += 1;
        }
        count[0] = 0;
        let mut offsets = [0u16; 16];
        for len in 1..15 {
            offsets[len + 1] = offsets[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbol[offsets[len as usize] as usize] = sym as u16;
                offsets[len as usize] += 1;
            }
        }
        Huffman { count, symbol }
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.count[len]);
            if code - count < first {
                return self.symbol.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

/// Enough of RFC 1950/1951 to read what LibVNCClient's deflate writes: stored,
/// fixed and dynamic blocks, ending at a final block or at the end of a stream that
/// was only sync-flushed.
fn zlib_inflate(data: &[u8]) -> Option<Vec<u8>> {
    const LBASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEXT: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DBASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DEXT: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    let mut bits = Bits {
        data: data.get(2..)?,
        pos: 0,
        buffer: 0,
        count: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    while let Some(last) = bits.take(1) {
        match bits.take(2)? {
            0 => {
                bits.buffer = 0;
                bits.count = 0;
                let header = bits.data.get(bits.pos..bits.pos + 4)?;
                let len = u16::from_le_bytes([header[0], header[1]]) as usize;
                bits.pos += 4;
                out.extend_from_slice(bits.data.get(bits.pos..bits.pos + len)?);
                bits.pos += len;
            }
            kind @ (1 | 2) => {
                let (lit, dist) =
                    if kind == 1 {
                        let mut lengths = [0u8; 288];
                        lengths[..144].fill(8);
                        lengths[144..256].fill(9);
                        lengths[256..280].fill(7);
                        lengths[280..].fill(8);
                        (Huffman::new(&lengths), Huffman::new(&[5u8; 30]))
                    } else {
                        let nlen = bits.take(5)? as usize + 257;
                        let ndist = bits.take(5)? as usize + 1;
                        let ncode = bits.take(4)? as usize + 4;
                        let mut code_lengths = [0u8; 19];
                        for &index in ORDER.iter().take(ncode) {
                            code_lengths[index] = bits.take(3)? as u8;
                        }
                        let codes = Huffman::new(&code_lengths);
                        let mut lengths = Vec::with_capacity(nlen + ndist);
                        while lengths.len() < nlen + ndist {
                            match codes.decode(&mut bits)? {
                                sym @ 0..=15 => lengths.push(sym as u8),
                                16 => {
                                    let previous = *lengths.last()?;
                                    for _ in 0..3 + bits.take(2)? {
                                        lengths.push(previous);
                                    }
                                }
                                17 => lengths
                                    .extend(std::iter::repeat_n(0, 3 + bits.take(3)? as usize)),
                                _ => lengths
                                    .extend(std::iter::repeat_n(0, 11 + bits.take(7)? as usize)),
                            }
                        }
                        (
                            Huffman::new(&lengths[..nlen]),
                            Huffman::new(&lengths[nlen..nlen + ndist]),
                        )
                    };
                loop {
                    let sym = lit.decode(&mut bits)? as usize;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let index = sym - 257;
                        let len = *LBASE.get(index)? as usize
                            + bits.take(u32::from(*LEXT.get(index)?))? as usize;
                        let code = dist.decode(&mut bits)? as usize;
                        let back = *DBASE.get(code)? as usize
                            + bits.take(u32::from(*DEXT.get(code)?))? as usize;
                        let start = out.len().checked_sub(back)?;
                        for i in 0..len {
                            out.push(out[start + i]);
                        }
                    }
                }
            }
            _ => return None,
        }
        if last == 1 {
            break;
        }
    }
    Some(out)
}

fn texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Clipboard { entries } => entries
                .iter()
                .find_map(|entry| entry.as_text())
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn notices(events: &[Event]) -> Vec<Notice> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Notice { notice } => Some(*notice),
            _ => None,
        })
        .collect()
}

/// Retries a call that needs the Connected state the shim only reports through
/// the queue (which the caller has already drained).
fn when_connected(
    client: &mut DesktopClient,
    mut call: impl FnMut(&DesktopClient) -> Result<(), Error>,
) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match call(client) {
            Ok(()) => return,
            Err(Error::State) if Instant::now() < deadline => {
                client.track_state();
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("the call never became valid: {error}"),
        }
    }
}

// ------------------------------------------------------------------------ tests

#[test]
fn the_event_struct_is_the_one_the_header_declares() {
    assert_eq!(shellune_desktop::SD_ABI_VERSION, 2);
    // Same numbers as the layout assertions in shim/sd_shim.c.
    assert_eq!(
        shellune_desktop::abi_layout(),
        [136, 32, 48, 112, 120, 132, 16]
    );
}

#[test]
fn an_oversized_desktop_is_refused_before_the_backend_allocates_it() {
    if !backends_built() {
        return;
    }
    for protocol in [Protocol::Rdp, Protocol::Vnc] {
        let mut client = DesktopClient::new(protocol).unwrap();
        assert!(matches!(client.set_size(8192, 8192), Err(Error::Argument)));
        client.set_size(3840, 2160).unwrap();
    }
}

#[test]
fn a_vnc_banner_delayed_three_seconds_still_connects() {
    if !backends_built() {
        return;
    }
    let peer = Scripted::start_after(
        Script::Session {
            width: 4,
            height: 3,
            pixel: [1, 2, 3, 0],
            clipboard: None,
            extended: false,
        },
        Duration::from_secs(3),
    );
    let mut client = DesktopClient::new(Protocol::Vnc).unwrap();
    client.set_server("127.0.0.1", peer.port).unwrap();
    client.connect().unwrap();
    let events = pump(&client, PATIENCE, |event| {
        matches!(event, Event::Frame { .. })
    });
    assert!(states(&events).contains(&State::Connected), "{events:?}");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Frame { .. })),
        "{events:?}"
    );
    let start = Instant::now();
    client.cancel().unwrap();
    client.wait_for_worker(Duration::from_secs(2)).unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_closed_vnc_port_is_a_connect_failure_never_a_connection() {
    if !backends_built() {
        return;
    }
    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client
        .set_server("127.0.0.1", a_closed_port())
        .expect("server");
    client.connect().expect("connect starts");

    let events = pump(&client, PATIENCE, |event| {
        matches!(
            event,
            Event::State {
                state: State::Failed(_),
                ..
            }
        )
    });
    never_connected(&events);
    assert_eq!(
        states(&events).last(),
        Some(&State::Failed(Failure::Connect)),
        "{events:?}"
    );
    assert_eq!(leaked_handles(), 0);
}

#[test]
fn a_silent_vnc_peer_can_be_cancelled_while_the_banner_never_arrives() {
    if !backends_built() {
        return;
    }
    let peer = Scripted::start(Script::Silent);
    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    client.connect().expect("connect starts");

    // Wait until the shim is actually talking to the peer.
    let deadline = Instant::now() + PATIENCE;
    while !peer.accepted.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    client.cancel().expect("cancel");
    let events = pump(&client, PATIENCE, |event| {
        matches!(
            event,
            Event::State {
                state: State::Closed,
                ..
            }
        )
    });

    never_connected(&events);
    assert_eq!(
        states(&events).last(),
        Some(&State::Closed),
        "a cancelled connect ends in Closed: {events:?}"
    );
    client.wait_for_worker(PATIENCE).expect("the worker stops");
    assert_eq!(leaked_handles(), 0);
}

#[test]
fn a_cancelled_rdp_connect_stops_without_reporting_a_session() {
    if !backends_built() {
        return;
    }
    let peer = Scripted::start(Script::Silent);
    let mut client = DesktopClient::new(Protocol::Rdp).expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    client
        .set_credentials(Some("probe"), Some("probe"), None)
        .expect("credentials");
    client.set_size(800, 600).expect("size");
    client.connect().expect("connect starts");

    let deadline = Instant::now() + PATIENCE;
    while !peer.accepted.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    // FreeRDP is now waiting for an X.224 response that never comes.
    thread::sleep(Duration::from_millis(200));
    client.cancel().expect("cancel");
    let started = Instant::now();
    client.wait_for_worker(PATIENCE).expect("the worker stops");
    assert!(
        started.elapsed() < PATIENCE,
        "aborting a pending connect must not wait for the timeout"
    );

    let events = pump(&client, Duration::from_millis(300), |_| false);
    never_connected(&events);
}

#[test]
fn an_unsupported_vnc_security_type_fails_instead_of_downgrading() {
    if !backends_built() {
        return;
    }
    // 18 is TLS. A build that cannot offer TLS must refuse the session, not fall
    // back to an unencrypted one and not report Connected.
    let peer = Scripted::start(Script::SecurityTypes(vec![18]));
    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    client
        .set_vnc_security(VncSecurity::DEFAULT)
        .expect("security mask");
    client.connect().expect("connect starts");

    let events = pump(&client, PATIENCE, |event| {
        matches!(
            event,
            Event::State {
                state: State::Failed(_),
                ..
            }
        )
    });
    never_connected(&events);
    assert_eq!(
        states(&events).last(),
        Some(&State::Failed(Failure::UnsupportedSecurity)),
        "{events:?}"
    );
}

/// The ABI accepts exactly one VNC pixel depth. The shim pins a 32bpp pixel format
/// and always hands out four-byte pixels, so a three-byte request must be refused
/// rather than accepted and then silently produce no frames.
#[test]
fn the_vnc_pixel_depth_is_fixed_at_four_bytes() {
    if !backends_built() {
        return;
    }
    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    assert!(
        matches!(client.set_vnc_colour_depth(3), Err(Error::Argument)),
        "three bytes per pixel is not a depth this backend can deliver"
    );
    assert!(matches!(
        client.set_vnc_colour_depth(0),
        Err(Error::Argument)
    ));
    assert!(matches!(client.set_vnc_colour_depth(4), Ok(())));
}

/// A push is answered by the switch first and by the state second, for both
/// protocols: RDP forwards the clipboard now, so an idle RDP handle reports the
/// state like VNC does, and a connection with sharing off says so instead of
/// queueing a group it would drop.
#[test]
fn clipboard_pushes_answer_by_switch_then_state_and_check_the_group() {
    if !backends_built() {
        return;
    }
    for protocol in [Protocol::Rdp, Protocol::Vnc] {
        let mut client = DesktopClient::new(protocol).expect("client");
        client
            .set_server("127.0.0.1", a_closed_port())
            .expect("server");
        assert!(
            matches!(client.clipboard("hello"), Err(Error::State)),
            "{protocol:?}: a handle that is not connected reports the state"
        );
        // Group rules come before the state: a bad group is never queued.
        let twice = [ClipboardEntry::text("a"), ClipboardEntry::text("b")];
        assert!(matches!(
            client.push_clipboard(&twice),
            Err(Error::Argument)
        ));
        let empty_image = [ClipboardEntry::png(Vec::new())];
        assert!(matches!(
            client.push_clipboard(&empty_image),
            Err(Error::Argument)
        ));
        // Text over its bound is cut, not refused (R1), so it gets as far as the
        // state; an image over its bound is refused.
        let long = "x".repeat(shellune_desktop::MAX_CLIPBOARD_TEXT_BYTES + 1);
        assert!(matches!(client.clipboard(&long), Err(Error::State)));
        let huge = [ClipboardEntry::png(vec![
            0;
            shellune_desktop::MAX_CLIPBOARD_IMAGE_BYTES
                + 1
        ])];
        assert!(matches!(client.push_clipboard(&huge), Err(Error::Argument)));
        let invalid = [ClipboardEntry {
            kind: ClipboardKind::Text,
            bytes: vec![0xFF, 0xFE],
        }];
        assert!(matches!(
            client.push_clipboard(&invalid),
            Err(Error::Argument)
        ));

        client.set_clipboard(false).expect("switch off");
        match client.clipboard("hello") {
            Err(Error::Unavailable(reason)) => assert!(reason.contains("clipboard"), "{reason}"),
            other => panic!("{protocol:?}: sharing off must be reported, not {other:?}"),
        }
    }
}

// ----------------------------------------------- RDP drive (patched FreeRDP)

const IRP_MJ_CREATE: u32 = 0x00;
const IRP_MJ_SET_INFORMATION: u32 = 0x06;
const IRP_MJ_DIRECTORY_CONTROL: u32 = 0x0C;
const IRP_MN_QUERY_DIRECTORY: u32 = 0x01;
const FILE_RENAME_INFORMATION: u32 = 10;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_DELETE_ON_CLOSE: u32 = 0x0000_1000;
const STATUS_SUCCESS: u32 = 0;
const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;

fn wide_path(path: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = path.encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// DR_DRIVE_CREATE_REQ after the common header ([MS-RDPEFS] 2.2.1.4.1).
fn create_request(path: &str, disposition: u32, options: u32) -> Vec<u8> {
    let name = wide_path(path);
    let mut body = Vec::new();
    body.extend_from_slice(&0x0013_019Fu32.to_le_bytes()); // DesiredAccess: read, write, delete
    body.extend_from_slice(&0u64.to_le_bytes()); // AllocationSize
    body.extend_from_slice(&0u32.to_le_bytes()); // FileAttributes
    body.extend_from_slice(&7u32.to_le_bytes()); // SharedAccess
    body.extend_from_slice(&disposition.to_le_bytes());
    body.extend_from_slice(&options.to_le_bytes());
    body.extend_from_slice(&(name.len() as u32).to_le_bytes());
    body.extend_from_slice(&name);
    body
}

/// DR_DRIVE_QUERY_DIRECTORY_REQ after the common header: the first query of a listing.
fn query_directory_request(path: &str) -> Vec<u8> {
    let name = wide_path(path);
    let mut body = Vec::new();
    body.extend_from_slice(&3u32.to_le_bytes()); // FileBothDirectoryInformation
    body.push(1); // InitialQuery
    body.extend_from_slice(&(name.len() as u32).to_le_bytes());
    body.extend_from_slice(&[0u8; 23]);
    body.extend_from_slice(&name);
    body
}

/// DR_DRIVE_SET_INFORMATION_REQ with FILE_RENAME_INFORMATION after the common header.
fn rename_request(target: &str) -> Vec<u8> {
    let name = wide_path(target);
    let mut buffer = vec![0u8, 0]; // ReplaceIfExists, RootDirectory
    buffer.extend_from_slice(&(name.len() as u32).to_le_bytes());
    buffer.extend_from_slice(&name);
    let mut body = Vec::new();
    body.extend_from_slice(&FILE_RENAME_INFORMATION.to_le_bytes());
    body.extend_from_slice(&(buffer.len() as u32).to_le_bytes());
    body.extend_from_slice(&[0u8; 24]);
    body.extend_from_slice(&buffer);
    body
}

fn create(body: &[u8]) -> DriveRequest<'_> {
    DriveRequest {
        major: IRP_MJ_CREATE,
        minor: 0,
        on_file: false,
        body,
    }
}

/// The statuses the patched drive channel answers `requests` with.
fn drive(base: &str, requests: &[DriveRequest<'_>]) -> Vec<u32> {
    probe::rdp_drive_requests(base, requests).expect("the RDP backend is built")
}

/// What one create request is answered with.
fn open_status(base: &str, path: &str, disposition: u32, options: u32) -> u32 {
    let body = create_request(path, disposition, options);
    drive(base, &[create(&body)])[0]
}

/// Makes `link` a link to the folder `target`: a junction on Windows, which needs no
/// privilege unlike a symbolic link, and a symbolic link elsewhere.
fn link_folder(target: &std::path::Path, link: &std::path::Path) {
    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("run mklink");
        assert!(made.status.success(), "mklink /J: {made:?}");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symbolic link to the folder");
}

/// Removes a link made by `link_folder`, never what it points at.
fn unlink_folder(link: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::fs::remove_dir(link);
    #[cfg(unix)]
    let _ = std::fs::remove_file(link);
}

/// Makes `link` a symbolic link to the file `target`; false when this account may not.
fn link_file(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    made.is_ok()
}

/// The RDP handle reports its own engine and the options an attempt would use.
#[test]
fn the_rdp_handle_reports_its_engine_and_options() {
    let mut client = DesktopClient::new(Protocol::Rdp).expect("client");
    let info = backend_info(Protocol::Rdp).expect("rdp info");
    assert!(info.contains("shellune"), "{info}");
    assert!(!info.contains("freerdp"), "{info}");

    let defaults = client.describe_rdp_settings().expect("settings");
    assert!(defaults.contains("implementation=shellune"), "{defaults}");
    assert!(defaults.contains("clipboard=1"), "{defaults}");
    assert!(defaults.contains("drive=0"), "{defaults}");

    client.set_clipboard(false).expect("switch off");
    assert!(
        client
            .describe_rdp_settings()
            .expect("settings")
            .contains("clipboard=0")
    );
    let folder = std::env::temp_dir();
    client
        .set_drive(
            shellune_desktop::DRIVE_NAME,
            Some(folder.to_str().expect("utf-8")),
        )
        .expect("drive");
    assert!(
        client
            .describe_rdp_settings()
            .expect("settings")
            .contains("drive=1")
    );
    client
        .set_drive(shellune_desktop::DRIVE_NAME, None)
        .expect("clear");
    assert!(
        client
            .describe_rdp_settings()
            .expect("settings")
            .contains("drive=0")
    );
}

/// A folder that cannot be offered is reported by the check an attempt runs.
#[test]
fn a_folder_that_cannot_be_offered_is_reported_with_its_reason() {
    let folder = std::env::temp_dir().join(format!("shellune-gone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    assert_eq!(
        probe::rdp_drive_folder(folder.to_str().expect("utf-8")),
        Some(Some(DriveProblem::Missing))
    );
    assert_eq!(
        probe::rdp_drive_folder("relative\\path"),
        Some(Some(DriveProblem::NotAbsolute))
    );
    let usable = std::env::temp_dir();
    assert_eq!(
        probe::rdp_drive_folder(usable.to_str().expect("utf-8")),
        Some(None)
    );
}

/// A missing folder does not stop the attempt: the connection is still dialled.
#[test]
fn an_rdp_attempt_with_a_missing_folder_still_dials() {
    let peer = Scripted::start(Script::Silent);
    let mut client = DesktopClient::new(Protocol::Rdp).expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    let missing = std::env::temp_dir().join("shellune-no-such-folder-for-this-test");
    client
        .set_drive(
            shellune_desktop::DRIVE_NAME,
            Some(missing.to_str().expect("utf-8")),
        )
        .expect("drive");
    client.connect().expect("connect starts");
    let deadline = Instant::now() + PATIENCE;
    while !peer.accepted.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        peer.accepted.load(Ordering::SeqCst),
        "the attempt still dialled the server"
    );
    let _ = client.cancel();
}

// ------------------------------------------------------- RDP clipboard channel
//
// These run the session's own cliprdr code over canned messages. They prove what
// goes on the wire and what the event stream receives, without a server.

const CF_DIB: u16 = 8;
const CF_UNICODETEXT: u16 = 13;

fn utf16(text: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// The CLIPRDR messages inside one session reply, as (kind, flags, data).
fn cliprdr_replies(bytes: &[u8]) -> Vec<(u16, u16, Vec<u8>)> {
    let mut replies = Vec::new();
    let mut at = 0usize;
    while at + 4 <= bytes.len() && bytes[at] == 3 {
        let total = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if total < 11 || at + total > bytes.len() {
            break;
        }
        // TPKT (4) + X.224 data header (3) + MCS SendDataRequest (0x64).
        let mcs = &bytes[at + 7..at + total];
        if mcs.first() != Some(&0x64) || mcs.len() < 7 {
            break;
        }
        let mut index = 5usize;
        if mcs[index] != 0x70 {
            break;
        }
        index += 1;
        let mut length = usize::from(mcs[index]);
        index += 1;
        if length & 0x80 != 0 {
            length = ((length & 0x7f) << 8) | usize::from(mcs[index]);
            index += 1;
        }
        if index + length > mcs.len() {
            break;
        }
        let value = &mcs[index..index + length];
        if value.len() >= 8 {
            let kind = u16::from_le_bytes([value[0], value[1]]);
            let flags = u16::from_le_bytes([value[2], value[3]]);
            let size = u32::from_le_bytes([value[4], value[5], value[6], value[7]]) as usize;
            let data = value[8..(8 + size).min(value.len())].to_vec();
            replies.push((kind, flags, data));
        }
        at += total;
    }
    replies
}

/// A local copy offers text as CF_UNICODETEXT and an image as CF_DIB, in the order
/// the caller gave them; an image is never offered as a PNG, and one over the bound
/// is refused instead of being truncated.
#[test]
fn an_rdp_copy_offers_text_and_a_dib_and_never_a_png() {
    let offered = probe::rdp_clipboard_offer(&[
        ClipboardEntry::text("a\nb"),
        ClipboardEntry::dib(vec![1, 2, 3]),
    ]);
    assert_eq!(
        offered,
        [(CF_UNICODETEXT, utf16("a\r\nb")), (CF_DIB, vec![1, 2, 3])]
    );
    assert!(
        probe::rdp_clipboard_offer(&[ClipboardEntry::png(vec![0x89, b'P', b'N', b'G'])]).is_empty(),
        "an RDP clipboard carries images as a DIB"
    );
    let huge = vec![0u8; shellune_desktop::MAX_CLIPBOARD_IMAGE_BYTES + 1];
    assert!(probe::rdp_clipboard_offer(&[ClipboardEntry::dib(huge)]).is_empty());
}

/// A remote copy is answered with one request, for the first format this client can
/// carry: text before any image.
#[test]
fn a_remote_rdp_format_list_is_answered_with_one_request() {
    let mut list = Vec::new();
    for format in [0x1234u16, CF_UNICODETEXT, CF_DIB] {
        list.extend_from_slice(&format.to_le_bytes());
    }
    let (events, replies) = probe::rdp_clipboard_receive(&[], &[(0x0002, 0, list)]);
    assert!(events.is_empty(), "{events:?}");
    let messages = cliprdr_replies(&replies);
    assert_eq!(
        messages.len(),
        2,
        "a request and the list response: {messages:?}"
    );
    assert_eq!(messages[0].0, 0x0004, "CB_FORMAT_DATA_REQUEST");
    assert_eq!(
        u32::from_le_bytes([
            messages[0].2[0],
            messages[0].2[1],
            messages[0].2[2],
            messages[0].2[3]
        ]),
        u32::from(CF_UNICODETEXT)
    );
    assert_eq!(messages[1].0, 0x0003, "CB_FORMAT_LIST_RESPONSE");
    assert_eq!(messages[1].1, 0x0001, "CB_RESPONSE_OK");
}

/// The answer to that request becomes one clipboard event; an incoming request is
/// served from what the last local copy offered.
#[test]
fn an_rdp_clipboard_answer_becomes_one_event_and_a_request_is_served() {
    // A remote copy is announced, requested, and only then answered.
    let copy = CF_UNICODETEXT.to_le_bytes().to_vec();
    let (events, _) = probe::rdp_clipboard_receive(
        &[],
        &[(0x0002, 0, copy), (0x0005, 0x0001, utf16("remote\r\ntext"))],
    );
    assert_eq!(
        events,
        [Event::Clipboard {
            entries: vec![ClipboardEntry::text("remote\r\ntext")]
        }]
    );

    let offered = [ClipboardEntry::text("local")];
    let mut request = CF_UNICODETEXT.to_le_bytes().to_vec();
    request.extend_from_slice(&[0, 0]);
    let (_, replies) = probe::rdp_clipboard_receive(&offered, &[(0x0004, 0, request)]);
    let messages = cliprdr_replies(&replies);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].0, 0x0005, "CB_FORMAT_DATA_RESPONSE");
    assert_eq!(messages[0].1, 0x0001, "CB_RESPONSE_OK");
    assert_eq!(messages[0].2, utf16("local"));
}

/// An image over its bound is dropped and reported; text over its bound is cut at
/// the last whole character, delivered, and reported the same way.
#[test]
fn an_oversized_remote_rdp_entry_is_cut_or_reported() {
    let image = vec![7u8; shellune_desktop::MAX_CLIPBOARD_IMAGE_BYTES + 1];
    let (events, _) = probe::rdp_clipboard_receive(
        &[],
        &[
            (0x0002, 0, CF_DIB.to_le_bytes().to_vec()),
            (0x0005, 0x0001, image),
        ],
    );
    assert_eq!(
        events,
        [Event::Notice {
            notice: Notice::ClipboardTooLarge(ClipboardKind::Dib)
        }]
    );

    let bound = shellune_desktop::MAX_CLIPBOARD_TEXT_BYTES;
    let text = utf16(&format!("a{}", "é".repeat(bound / 2)));
    let (events, _) = probe::rdp_clipboard_receive(
        &[],
        &[
            (0x0002, 0, CF_UNICODETEXT.to_le_bytes().to_vec()),
            (0x0005, 0x0001, text),
        ],
    );
    let expected = format!("a{}", "é".repeat(bound / 2 - 1));
    assert_eq!(
        events,
        [
            Event::Clipboard {
                entries: vec![ClipboardEntry::text(&expected)],
            },
            Event::Notice {
                notice: Notice::ClipboardTooLarge(ClipboardKind::Text)
            },
        ]
    );
}

/// A server that declines the device says so, once, with the code it gave; a
/// server that takes it says nothing.
#[test]
fn a_declined_drive_is_reported_with_how_it_was_declined() {
    let folder = std::env::temp_dir();
    let path = folder.to_str().expect("utf-8");
    assert_eq!(probe::rdp_drive_reply(path, 0), []);
    assert_eq!(
        probe::rdp_drive_reply(path, 0xC000_0022),
        [Event::Notice {
            notice: Notice::DriveRefused(DriveRefusal::Declined(0xC000_0022))
        }]
    );
}

/// A refused format list is reported, and the server offering formats again
/// withdraws the report.
#[test]
fn a_refused_rdp_clipboard_is_reported_and_withdrawn() {
    // A format list first, so the client is waiting for the answer.
    let copy = CF_UNICODETEXT.to_le_bytes().to_vec();
    let (events, _) =
        probe::rdp_clipboard_receive(&[], &[(0x0002, 0, copy), (0x0003, 0x0002, Vec::new())]);
    assert_eq!(
        events,
        [Event::Notice {
            notice: Notice::ClipboardRefused
        }]
    );

    // The server offers a format list again: the clipboard is usable.
    let mut list = Vec::new();
    for format in [CF_UNICODETEXT] {
        list.extend_from_slice(&format.to_le_bytes());
    }
    let (events, _) = probe::rdp_clipboard_receive(
        &[],
        &[
            (0x0002, 0, CF_UNICODETEXT.to_le_bytes().to_vec()),
            (0x0003, 0x0002, Vec::new()),
            (0x0002, 0, list),
        ],
    );
    assert_eq!(
        events,
        [
            Event::Notice {
                notice: Notice::ClipboardRefused
            },
            Event::Notice {
                notice: Notice::ClipboardReady
            },
        ]
    );
}

/// Text over the local bound is cut and reported when it is offered.
#[test]
fn an_oversized_local_rdp_copy_is_cut_and_reported() {
    let bound = shellune_desktop::MAX_CLIPBOARD_TEXT_BYTES;
    let (_, events) =
        probe::rdp_clipboard_offer_events(&[ClipboardEntry::text(&"x".repeat(bound + 1))]);
    assert_eq!(
        events,
        [Event::Notice {
            notice: Notice::ClipboardTooLarge(ClipboardKind::Text)
        }]
    );
    let (_, quiet) = probe::rdp_clipboard_offer_events(&[ClipboardEntry::text("short")]);
    assert!(quiet.is_empty(), "{quiet:?}");
}

/// A mapped folder with a junction in it that points outside. Through FreeRDP's own
/// drive channel, patched (packaging/native/patches/freerdp-drive-confinement.patch):
/// the server can use the folder, and nothing that resolves through the junction -
/// opening, creating, deleting, listing, or moving something into it - reaches the
/// place it points at (R5).
#[test]
fn the_patched_drive_channel_keeps_the_server_inside_the_mapped_folder() {
    if !backends_built() {
        return;
    }
    let root = std::env::temp_dir().join(format!("shellune-confine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let base = root.join("mapped");
    let outside = root.join("outside");
    std::fs::create_dir_all(base.join("inside")).expect("mapped folder");
    std::fs::create_dir_all(&outside).expect("outside folder");
    std::fs::write(outside.join("secret.txt"), b"secret").expect("outside file");
    std::fs::write(base.join("f.txt"), b"mine").expect("inside file");
    link_folder(&outside, &base.join("out"));
    let base_text = base.to_str().expect("utf-8 temp path").to_owned();
    let open = |path: &str| open_status(&base_text, path, FILE_OPEN, 0);
    // Windows and a default macOS volume ignore case; a Linux file system does not,
    // and there a differently spelled name is simply another, missing, file.
    let case_blind = base.join("F.TXT").exists();

    // Ordinary use of the folder is served.
    for path in ["\\", "\\f.txt", "/f.txt", "\\inside"] {
        assert_eq!(open(path), STATUS_SUCCESS, "{path:?} is inside");
    }
    if case_blind {
        assert_eq!(open("\\F.TXT"), STATUS_SUCCESS, "\\F.TXT is inside");
    }
    // Through the link: refused, however it is spelled.
    for path in [
        "\\out",
        "\\out\\secret.txt",
        "/out/secret.txt",
        "\\\\out\\\\secret.txt",
    ] {
        assert_eq!(
            open(path),
            STATUS_ACCESS_DENIED,
            "{path:?} resolves outside"
        );
    }
    if case_blind {
        assert_eq!(open("\\OUT\\SECRET.TXT"), STATUS_ACCESS_DENIED);
    } else {
        assert_ne!(open("\\OUT\\SECRET.TXT"), STATUS_SUCCESS);
    }
    // FreeRDP's own ".." refusal still holds, and a device name never opens.
    for path in [
        "\\inside\\..\\out\\secret.txt",
        "\\..\\outside\\secret.txt",
        "\\CON",
    ] {
        assert_ne!(open(path), STATUS_SUCCESS, "{path:?}");
    }

    // Creating through the junction: refused, and nothing appears outside.
    assert_eq!(
        open_status(&base_text, "\\out\\new.txt", FILE_CREATE, 0),
        STATUS_ACCESS_DENIED
    );
    assert!(!outside.join("new.txt").exists());
    assert_eq!(
        open_status(
            &base_text,
            "\\out\\newdir",
            FILE_CREATE,
            FILE_DIRECTORY_FILE
        ),
        STATUS_ACCESS_DENIED
    );
    assert!(!outside.join("newdir").exists());
    // ...while inside it works.
    assert_eq!(
        open_status(&base_text, "\\inside\\new.txt", FILE_CREATE, 0),
        STATUS_SUCCESS
    );
    assert!(base.join("inside").join("new.txt").exists());

    // Delete on close: a refused open deletes nothing (FreeRDP used to delete the
    // named path even when the open failed); inside, the file goes when it closes.
    assert_eq!(
        open_status(
            &base_text,
            "\\out\\secret.txt",
            FILE_OPEN,
            FILE_DELETE_ON_CLOSE
        ),
        STATUS_ACCESS_DENIED
    );
    assert!(
        outside.join("secret.txt").exists(),
        "the outside file survives"
    );
    assert_eq!(
        open_status(
            &base_text,
            "\\inside\\new.txt",
            FILE_OPEN,
            FILE_DELETE_ON_CLOSE
        ),
        STATUS_SUCCESS
    );
    assert!(!base.join("inside").join("new.txt").exists());

    // Listing: a directory inside is listed, the one behind the junction is not.
    let root_dir = create_request("\\", FILE_OPEN, FILE_DIRECTORY_FILE);
    let list = |pattern: &str| {
        let query = query_directory_request(pattern);
        drive(
            &base_text,
            &[
                create(&root_dir),
                DriveRequest {
                    major: IRP_MJ_DIRECTORY_CONTROL,
                    minor: IRP_MN_QUERY_DIRECTORY,
                    on_file: true,
                    body: &query,
                },
            ],
        )
    };
    assert_eq!(list("\\inside\\*"), [STATUS_SUCCESS, STATUS_SUCCESS]);
    assert_eq!(list("\\out\\*"), [STATUS_SUCCESS, STATUS_ACCESS_DENIED]);

    // Renaming: never into the junction; within the folder, as usual.
    let file = create_request("\\f.txt", FILE_OPEN, 0);
    let rename = |target: &str| {
        let body = rename_request(target);
        drive(
            &base_text,
            &[
                create(&file),
                DriveRequest {
                    major: IRP_MJ_SET_INFORMATION,
                    minor: 0,
                    on_file: true,
                    body: &body,
                },
            ],
        )
    };
    assert_eq!(
        rename("\\out\\moved.txt"),
        [STATUS_SUCCESS, STATUS_ACCESS_DENIED]
    );
    assert!(!outside.join("moved.txt").exists());
    assert!(base.join("f.txt").exists());
    assert_eq!(
        rename("\\inside\\moved.txt"),
        [STATUS_SUCCESS, STATUS_SUCCESS]
    );
    assert!(base.join("inside").join("moved.txt").exists());

    // A symbolic link is refused like the junction, where this account may make one.
    if link_file(&outside.join("secret.txt"), &base.join("link.txt")) {
        assert_eq!(open("\\link.txt"), STATUS_ACCESS_DENIED);
    }

    // The link itself goes, its target stays.
    unlink_folder(&base.join("out"));
    assert!(outside.join("secret.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}
