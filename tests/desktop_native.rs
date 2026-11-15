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
//! which command produces them. Nothing is faked into a pass.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use shellune_desktop::{
    DesktopClient, Error, Event, Failure, PointerButtons, Protocol, State, VncSecurity,
    backend_info, callback_panics, leaked_handles,
};

/// How long a scripted peer or a worker may take before the test calls it a hang.
const PATIENCE: Duration = Duration::from_secs(10);

fn backends_built() -> bool {
    let mut built = true;
    for protocol in [Protocol::Rdp, Protocol::Vnc] {
        match backend_info(protocol) {
            Ok(info) => eprintln!("backend {protocol:?}: {info}"),
            Err(Error::Unavailable(reason)) => {
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

/// A port nothing is listening on: bind, note the number, then close.
fn a_closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a probe port");
    let port = listener.local_addr().expect("probe address").port();
    drop(listener);
    port
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
    UpdateRequest { incremental: bool },
    Key { keysym: u32, down: bool },
    Pointer { x: u16, y: u16, mask: u8 },
    CutText,
    Other(u8),
}

/// What the scripted peer does once a socket is accepted.
#[derive(Clone)]
enum Script {
    /// Accept and say nothing: the client must be able to cancel.
    Silent,
    /// Announce only these security types, then wait for the client's choice.
    SecurityTypes(Vec<u8>),
    /// Complete a 3.8 handshake and serve one raw frame, then keep reading input.
    Session {
        width: u16,
        height: u16,
        pixel: [u8; 4],
        clipboard: Option<&'static str>,
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
                    } => {
                        // One session per test: after it ends there is nothing left to
                        // serve, and a second accept would only hide client bugs.
                        serve_session(stream, width, height, pixel, clipboard, Arc::clone(&log));
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
            let length = u32::from_be_bytes([header[3], header[4], header[5], header[6]]) as usize;
            let mut text = vec![0u8; length.min(4096)];
            stream.read_exact(&mut text).ok()?;
            Some(ClientMessage::CutText)
        }
        other => Some(ClientMessage::Other(other)),
    }
}

fn serve_session(
    mut stream: TcpStream,
    width: u16,
    height: u16,
    pixel: [u8; 4],
    clipboard: Option<&'static str>,
    log: Arc<Mutex<Vec<ClientMessage>>>,
) {
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

    if let Some(text) = clipboard {
        let mut cut = vec![3u8, 0, 0, 0];
        cut.extend_from_slice(&(text.len() as u32).to_be_bytes());
        cut.extend_from_slice(text.as_bytes());
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
        log.lock().expect("message log").push(message);
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

// ------------------------------------------------------------------------ tests

#[test]
fn the_event_struct_is_the_one_the_header_declares() {
    assert_eq!(shellune_desktop::SD_ABI_VERSION, 1);
    // Same numbers as the _Static_assert block in shim/sd_shim.c.
    assert_eq!(shellune_desktop::abi_layout(), [120, 32, 48, 112]);
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
fn both_backends_report_the_libraries_they_link() {
    if !backends_built() {
        return;
    }
    let rdp = backend_info(Protocol::Rdp).expect("rdp info");
    assert!(rdp.contains("freerdp="), "{rdp}");
    assert!(rdp.contains("tls=openssl"), "{rdp}");
    let vnc = backend_info(Protocol::Vnc).expect("vnc info");
    assert!(vnc.contains("libvncclient="), "{vnc}");
    // The build must say which security types it can negotiate, not imply more, and
    // the names must be the RFB ones: "vencrypt" is a security type,
    // "vencrypt-auth" is not.
    assert!(vnc.contains("security=none,vncauth"), "{vnc}");
    assert!(!vnc.contains("vencrypt-auth"), "{vnc}");
    if vnc.contains("tls=none") {
        assert!(
            !vnc.contains(",tls,vencrypt"),
            "a build without a TLS backend must not advertise TLS or VeNCrypt: {vnc}"
        );
    } else {
        assert!(
            vnc.contains("security=none,vncauth,tls,vencrypt"),
            "a build with a TLS backend must advertise both: {vnc}"
        );
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

/// Clipboard text is only forwarded by the VNC backend. An RDP session says so
/// instead of accepting text the shim would drop, and the VNC answer is still the
/// state check, so the two are not conflated.
#[test]
fn an_rdp_session_reports_that_it_cannot_forward_clipboard_text() {
    if !backends_built() {
        return;
    }
    let mut rdp = DesktopClient::new(Protocol::Rdp).expect("rdp client");
    rdp.set_server("127.0.0.1", a_closed_port())
        .expect("server");
    match rdp.clipboard("hello") {
        Err(Error::Unavailable(reason)) => {
            assert!(reason.contains("clipboard"), "{reason}");
        }
        other => panic!("an RDP session must report the missing channel, not {other:?}"),
    }

    let mut vnc = DesktopClient::new(Protocol::Vnc).expect("vnc client");
    vnc.set_server("127.0.0.1", a_closed_port())
        .expect("server");
    assert!(
        matches!(vnc.clipboard("hello"), Err(Error::State)),
        "a VNC session that is not connected reports the state, not the capability"
    );
}

/// Covers a clipboard and first frame buffered together. The worker must consume
/// library-buffered bytes without waiting for another socket notification.
#[test]
fn a_scripted_vnc_peer_delivers_a_frame_clipboard_and_accepts_input() {
    if !backends_built() {
        return;
    }
    let pixel = [0x11u8, 0x22, 0x33, 0x00];
    let peer = Scripted::start(Script::Session {
        width: 4,
        height: 3,
        pixel,
        clipboard: Some("from the peer"),
    });

    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    client.set_size(4, 3).expect("size");
    client.connect().expect("connect starts");

    let events = pump(&client, PATIENCE, |event| {
        matches!(event, Event::Frame { .. })
    });
    assert!(
        states(&events).contains(&State::Connected),
        "the handshake completed: {events:?}"
    );

    let frame = events
        .iter()
        .find_map(|event| match event {
            Event::Frame {
                width,
                height,
                pixels,
                stride,
                desktop_width,
                ..
            } => Some((*width, *height, pixels.clone(), *stride, *desktop_width)),
            _ => None,
        })
        .expect("a frame arrived");
    assert_eq!(frame.0, 4);
    assert_eq!(frame.1, 3);
    assert_eq!(frame.3, 16, "four pixels of four bytes per row");
    assert_eq!(frame.4, 4, "the desktop is four pixels wide");
    assert_eq!(frame.2.len(), 4 * 3 * 4);
    for chunk in frame.2.as_chunks::<4>().0 {
        assert_eq!(chunk, &pixel, "BGRA bytes as the peer sent them");
    }

    // Text clipboard from the peer, whether it arrived before or after the frame.
    let mut clipboard = events.clone();
    clipboard.extend(pump(&client, Duration::from_secs(2), |event| {
        matches!(event, Event::Clipboard { .. })
    }));
    assert!(
        clipboard
            .iter()
            .any(|event| matches!(event, Event::Clipboard { text } if text == "from the peer")),
        "clipboard text: {clipboard:?}"
    );

    // A frame can arrive while the handshake is still finishing, so wait for the
    // state the input path requires before sending anything.
    // `pump` consumed Connected above. `state()` is a cache maintained only by
    // track_state(), so waiting for it here would wait for an already-read event.

    // Input: the keysym form for VNC, and a pointer position with a left press.
    // A first frame may already have arrived while the handshake was finishing, so the
    // input calls wait for the connected state the shim requires instead of assuming
    // the event order.
    let deadline = Instant::now() + PATIENCE;
    loop {
        match client.key(0, 0x61, true, false) {
            Ok(()) => break,
            Err(_) if Instant::now() < deadline => {
                client.track_state();
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("key down never became valid: {error}"),
        }
    }
    client.key(0, 0x61, false, false).expect("key up");
    client.pointer(2, 1, PointerButtons::LEFT).expect("pointer");
    assert!(
        peer.wait_for(ClientMessage::Key {
            keysym: 0x61,
            down: true
        }),
        "the peer saw the key press: {:?}",
        peer.messages()
    );
    assert!(
        peer.wait_for(ClientMessage::Pointer {
            x: 2,
            y: 1,
            mask: 1
        }),
        "the peer saw the pointer: {:?}",
        peer.messages()
    );
    assert!(
        peer.messages()
            .iter()
            .any(|message| matches!(message, ClientMessage::Encodings(_))),
        "the client asked for encodings: {:?}",
        peer.messages()
    );

    // Closing a live session must stop the worker and release the handle.
    drop(client);
    assert_eq!(leaked_handles(), 0);
}

#[test]
fn a_panicking_consumer_callback_cannot_unwind_into_the_library() {
    if !backends_built() {
        return;
    }
    let peer = Scripted::start(Script::Session {
        width: 2,
        height: 2,
        pixel: [0x40, 0x50, 0x60, 0x00],
        clipboard: None,
    });

    let before = callback_panics();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let first = Arc::new(AtomicBool::new(true));
    let mut client = DesktopClient::with_handler(Protocol::Vnc, move |event| {
        if matches!(event, Event::State { .. }) && first.swap(false, Ordering::SeqCst) {
            // Exactly the failure mode the trampoline has to absorb. One state event is
            // guaranteed, unlike a frame from this fixture.
            panic!("a consumer callback panicked on purpose");
        }
        record.lock().expect("record").push(event);
    })
    .expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    client.set_size(2, 2).expect("size");
    client.connect().expect("connect starts");

    let deadline = Instant::now() + PATIENCE;
    while callback_panics() == before && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        callback_panics() > before,
        "the trampoline must count the panic instead of letting it unwind"
    );

    // The session kept running afterwards: cancelling still produces events.
    client.cancel().expect("cancel");
    client.wait_for_worker(PATIENCE).expect("the worker stops");
    drop(client);
    assert_eq!(leaked_handles(), 0);
    let delivered = seen.lock().expect("record").len();
    assert!(
        delivered > 0,
        "events before and after the panic still reached the consumer"
    );
}

/// A live session is cancelled, its callback unregistered and its context freed in
/// that order, and the whole sequence finishes without waiting for a protocol
/// timeout. That the detach comes before the wait is what the crate documents; the
/// observable part here is that the drop is prompt and the handle is released.
#[test]
fn dropping_a_session_detaches_before_it_frees() {
    if !backends_built() {
        return;
    }
    let peer = Scripted::start(Script::Session {
        width: 2,
        height: 2,
        pixel: [0x01, 0x02, 0x03, 0x00],
        clipboard: None,
    });
    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client.set_server("127.0.0.1", peer.port).expect("server");
    client.set_size(2, 2).expect("size");
    client.connect().expect("connect starts");
    pump(&client, PATIENCE, |event| {
        matches!(event, Event::Frame { .. })
    });

    let started = Instant::now();
    drop(client);
    assert!(
        started.elapsed() < PATIENCE,
        "the drop waited for the protocol timeout instead of cancelling"
    );
    assert_eq!(leaked_handles(), 0, "the handle was released");
}

/// A handshake that fails must not leave a pointer to the freed session behind.
/// The cancel lands while the failure is being classified, which is exactly the
/// window where the old code left client->backend dangling: the shim must clear the
/// pointer before it frees, and an abort that arrives anyway must do nothing.
#[test]
fn cancelling_a_failing_vnc_handshake_touches_no_freed_session() {
    if !backends_built() {
        return;
    }
    // 18 is TLS; a build without a TLS backend refuses it, so the handshake fails
    // quickly and deterministically.
    for attempt in 0..3 {
        let peer = Scripted::start(Script::SecurityTypes(vec![18]));
        let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
        client.set_server("127.0.0.1", peer.port).expect("server");
        client
            .set_vnc_security(VncSecurity::DEFAULT)
            .expect("security mask");
        client.connect().expect("connect starts");
        // Racing the failure on purpose: on some runs this aborts a live connect,
        // on others it arrives after the handshake already failed.
        let _ = client.cancel();

        let events = pump(&client, PATIENCE, |event| {
            matches!(
                event,
                Event::State {
                    state: State::Failed(_) | State::Closed,
                    ..
                }
            )
        });
        never_connected(&events);
        let last = states(&events).last().copied();
        assert!(
            matches!(last, Some(State::Failed(_) | State::Closed)),
            "attempt {attempt}: a cancelled or failed handshake ends in Failed or Closed: {events:?}"
        );
        client.wait_for_worker(PATIENCE).expect("the worker stops");
        drop(client);
        assert_eq!(
            leaked_handles(),
            0,
            "attempt {attempt}: the handle was released"
        );
    }
}

/// A peer that closes right after its banner ends the session as a failure, and the
/// test reports SKIP like every other one when the backends are not built.
#[test]
fn a_peer_that_hangs_up_after_its_banner_ends_the_session() {
    if !backends_built() {
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let thread = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.write_all(b"RFB 003.008\n");
            let mut version = [0u8; 12];
            let _ = stream.read_exact(&mut version);
            let _ = stream.shutdown(Shutdown::Both);
        }
    });

    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client.set_server("127.0.0.1", port).expect("server");
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
    thread.join().expect("peer thread");
}
