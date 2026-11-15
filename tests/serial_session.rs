//! A serial session end to end: the port, the terminal and the rules between them
//! (R17, R21).
//!
//! Two kinds of check live here, and the names say which is which.
//!
//! * The tests that drive [`shellune_serial::testing`] run a real `SerialSession` - its
//!   reader thread, writer thread, bounded queue, partial writes and close - over an
//!   in-memory port, and put what arrives into the same `TerminalCore` the application
//!   draws. That is the whole pipeline below the window: bytes in, grid text out, with
//!   search, scrollback and a selection over it.
//! * The last test is the device matrix. It asks the machine for a real port and, when
//!   there is none, lists every item it could not verify instead of quietly passing.
//!
//! What this file cannot prove: this machine has no serial device and no com0com pair,
//! so nothing here is evidence about a UART, a cable, a driver, hardware flow control,
//! DTR/RTS behaviour or a physical unplug. An in-memory port is a test double, and every
//! assertion below is written as what it is.

use std::time::{Duration, Instant};

use shellune_serial::testing::{FakeSpec, fake, fake_pair, session_over, session_over_with};
use shellune_serial::{
    DataBits, FlowControl, Parity, SerialCause, SerialConfig, SerialEvent, SerialSession,
    SessionLimits, StopBits, list_ports,
};
use shellune_terminal::SelectionType;
use shellune_terminal::TerminalCore;
use shellune_terminal::core::SearchOptions;

/// How long a test waits for a thread to do its work before calling it a hang.
const PATIENCE: Duration = Duration::from_secs(5);

/// The non-default frame the pipeline tests open with.
///
/// Seven data bits, even parity and two stop bits: three of the five settings differ
/// from the default, so a mapping that silently used 8N1 would fail here.
fn non_default_frame() -> SerialConfig {
    SerialConfig::new(
        9600,
        DataBits::Seven,
        Parity::Even,
        StopBits::Two,
        FlowControl::Software,
    )
}

/// One serial session with the terminal that parses what it receives.
///
/// The pair is the crate in-memory one: what the session writes reaches the peer, and
/// what the peer is given arrives at the session. Nothing here is a device.
struct Rig {
    session: SerialSession,
    /// The port the session holds, so the test can read what it accepted.
    port: std::sync::Arc<shellune_serial::testing::FakePort>,
    /// The other end of the pair, kept so the pair is not broken by a dropped handle.
    _far_end: std::sync::Arc<shellune_serial::testing::FakePort>,
    core: TerminalCore,
    received: Vec<u8>,
}

impl Rig {
    fn open(config: SerialConfig) -> Self {
        let (port, far_end) = fake_pair();
        let session = session_over(std::sync::Arc::clone(&port), config);
        Self {
            session,
            port,
            _far_end: far_end,
            core: TerminalCore::new(80, 24, 2000),
            received: Vec::new(),
        }
    }

    /// Polls until `done` says the session has finished, or the patience runs out.
    fn pump_until(&mut self, mut done: impl FnMut(&Rig) -> bool) -> bool {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let events = self.session.poll();
            for event in events {
                if let SerialEvent::Received(bytes) = event {
                    self.received.extend_from_slice(&bytes);
                    self.core.feed(&bytes);
                }
            }
            if done(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The grid as text, including nothing the device did not send.
    fn text(&self) -> Vec<String> {
        self.core.snapshot().text()
    }
}

#[test]
fn a_non_default_frame_carries_ascii_chinese_and_binary_bytes_unchanged() {
    let mut rig = Rig::open(non_default_frame());
    // ASCII, a multi-byte character, a NUL and a sequence that is not valid UTF-8: the
    // four cases R21 asks the约定 bytes of. They are bytes on the wire, and the terminal
    // above this crate is the only thing that decides what text is.
    let from_device: Vec<u8> = vec![
        b'r', b'e', b'a', b'd', b'y', b'\r', b'\n', 0xe4, 0xb8, 0xad, 0xe6, 0x96, 0x87, b'\r',
        b'\n', // 中文
        0x00, b'A', b'\r', b'\n', 0xff, 0xfe, b'\r', b'\n',
    ];
    // What the device sends lands in the session inbox; the pair is a pair of inboxes,
    // not a pair of logs.
    rig.port.push_from_device(&from_device);
    assert!(
        rig.pump_until(|rig| rig.received.len() >= from_device.len()),
        "the device bytes arrived within {PATIENCE:?}: {} of {}",
        rig.received.len(),
        from_device.len()
    );
    assert_eq!(
        rig.received, from_device,
        "the bytes are delivered exactly as the device sent them"
    );
    let text = rig.text();
    assert!(
        text.iter().any(|row| row.contains("ready")),
        "the ASCII line reached the grid: {text:?}"
    );
    assert!(
        text.iter().any(|row| row.contains("中文")),
        "the Chinese text reached the grid: {text:?}"
    );

    // The other direction, with the same frame: what the user sends reaches the device
    // byte for byte, including a NUL and a non-UTF-8 sequence.
    let from_user: Vec<u8> = vec![b'o', b'k', 0x00, 0xc3, 0x28, b'\r'];
    rig.session
        .send(&from_user)
        .expect("the queue takes the frame");
    let deadline = Instant::now() + PATIENCE;
    while rig.port.written().len() < from_user.len() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        rig.port.written(),
        from_user,
        "the frame reached the port exactly as it was queued"
    );
    let _ = rig.session.close();
}

#[test]
fn the_grid_over_a_serial_session_can_be_searched_scrolled_and_copied() {
    let mut rig = Rig::open(non_default_frame());
    let mut sent = Vec::new();
    for line in 0..120 {
        sent.extend_from_slice(format!("row {line} payload\r\n").as_bytes());
    }
    rig.port.push_from_device(&sent);
    assert!(
        rig.pump_until(|rig| rig.received.len() >= sent.len()),
        "the burst arrived: {} of {}",
        rig.received.len(),
        sent.len()
    );

    // Search: every match carries its grid line and its text, which is what the find
    // bar scrolls to.
    let matches = rig
        .core
        .search_history("row 7", SearchOptions::literal())
        .expect("a literal query compiles");
    assert!(
        matches.len() >= 11,
        "each occurrence in the scrollback is its own match: {} found",
        matches.len()
    );

    // Scroll: the lines that left the screen are still there.
    assert!(
        rig.core.history() > 0,
        "the grid kept the lines that scrolled off"
    );
    rig.core.scroll(10);
    assert!(
        rig.core.display_offset() > 0,
        "the view can be moved into the scrollback"
    );
    rig.core.scroll_to_bottom();

    // Copy: a selection over one row reads back as that row.
    rig.core.begin_selection(0, 0, SelectionType::Simple);
    rig.core.extend_selection(0, 3);
    let copied = rig
        .core
        .selection_text()
        .expect("a selection over the first row has text");
    assert!(
        !copied.trim().is_empty(),
        "the selection reads back as text: {copied:?}"
    );
    let _ = rig.session.close();
}

#[test]
fn a_device_that_goes_away_ends_the_session_and_frees_the_port() {
    // A read that fails is what an unplugged device reports.
    let spec = FakeSpec::new().failing_reads(
        shellune_serial::SerialError::new(SerialCause::Disconnected)
            .at("fake-a")
            .with_detail("the device was removed"),
    );
    let port = fake("fake-a", spec);
    let mut session = session_over_with(
        std::sync::Arc::clone(&port),
        non_default_frame(),
        SessionLimits::default(),
    );
    let deadline = Instant::now() + PATIENCE;
    let mut disconnected = None;
    while Instant::now() < deadline {
        for event in session.poll() {
            if let SerialEvent::Disconnected { error } = event {
                disconnected = Some(error);
            }
        }
        if disconnected.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let error = disconnected.expect("the failure is reported as a disconnect");
    assert!(
        error.is(SerialCause::Disconnected),
        "the cause the platform allowed is kept: {error:?}"
    );
    // The port is released by the session itself: nothing is still held after a device
    // that went away, so the same port can be opened again once it is back. The release
    // is not done in the very step that hands the disconnect over - the session settles
    // into its final state first and lets the port go there - so it is waited for, the
    // same way every other state in this file is, instead of being read at one instant.
    // Measured: one full-suite run in four failed here on that single reading.
    let deadline = Instant::now() + PATIENCE;
    while !port.is_released() && Instant::now() < deadline {
        let _ = session.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        port.is_released(),
        "the port was released within {PATIENCE:?} of the device failing"
    );
    let _ = session.close();
}

#[test]
fn a_frame_the_driver_cut_short_is_reported_and_never_replayed() {
    // A driver that accepts nothing: the frame stays in flight until the session closes.
    let port = fake("fake-a", FakeSpec::new().holding_writes());
    let mut session = session_over_with(
        std::sync::Arc::clone(&port),
        non_default_frame(),
        SessionLimits::default(),
    );
    session.send(b"half a frame").expect("the queue takes it");
    // The writer takes the frame before the close: a frame the driver is already inside
    // is the case that is reported part way, while a frame still in the queue is a
    // discarded count instead. Waiting for the hand-over keeps the assertion on the
    // first case rather than on the race between them.
    let deadline = Instant::now() + PATIENCE;
    while session.pending_frames() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = session.close();
    let events = session.poll();
    let incomplete = events.iter().find_map(|event| match event {
        SerialEvent::SendIncomplete {
            sent, requested, ..
        } => Some((*sent, *requested)),
        _ => None,
    });
    assert_eq!(
        incomplete,
        Some((0, 12)),
        "the frame that was not written is reported part way: {events:?}"
    );
    assert!(
        port.written().is_empty(),
        "and none of it is written later: {:?}",
        port.written()
    );
}

/// The real-device matrix, and the honest report of what could not be run (R21).
///
/// Every item is either verified here or listed with the reason it was not. Nothing is
/// skipped silently, and a matrix full of unverified items is not a pass: the report is
/// printed so a reader of the task result sees exactly what a machine without a device
/// could and could not prove.
#[test]
fn the_real_device_matrix_reports_every_item_it_could_not_verify() {
    let port_name = std::env::var("SHELLUNE_SERIAL_PORT").ok();
    let peer = std::env::var("SHELLUNE_SERIAL_PEER_PORT").ok();
    let listed = list_ports();
    let mut report = Vec::new();
    match &listed {
        Ok(ports) => report.push((
            "the port list came from the system",
            if ports.is_empty() {
                format!(
                    "verified: the system answered with no ports ({})",
                    ports.len()
                )
            } else {
                format!("verified: the system listed {} ports", ports.len())
            },
        )),
        Err(error) => report.push((
            "the port list came from the system",
            format!("unverified: the list could not be read ({error})"),
        )),
    }
    let frame = non_default_frame();
    match &port_name {
        Some(name) => match SerialSession::open(name, &frame) {
            Ok(session) => {
                report.push((
                    "a real device opens with a non-default frame",
                    format!("verified: {name} opened as {}", frame.summary()),
                ));
                let _ = session;
            }
            Err(error) => report.push((
                "a real device opens with a non-default frame",
                format!("unverified: {name} did not open ({error})"),
            )),
        },
        None => report.push((
            "a real device opens with a non-default frame",
            "unverified: SHELLUNE_SERIAL_PORT is not set, and this machine has no serial device"
                .to_owned(),
        )),
    }
    for item in [
        "two devices carry bytes in both directions with hardware flow control",
        "DTR and RTS behave as configured on a real adapter",
        "a physical unplug is reported as a disconnect and the port is released",
        "a replug reopens the same port under the same name",
    ] {
        let reason = match (&peer, item) {
            (None, _) => "unverified: needs a second device or a null-modem pair, which this machine does not have".to_owned(),
            (Some(_), _) => "unverified: a second port is configured but this build has no scripted check for it".to_owned(),
        };
        report.push((item, reason));
    }
    for (item, outcome) in &report {
        eprintln!("{item}: {outcome}");
    }
    assert_eq!(
        report.len(),
        6,
        "every matrix item is reported, so nothing is skipped silently"
    );
    assert!(
        report
            .iter()
            .all(|(_, outcome)| outcome.starts_with("verified: ")
                || outcome.starts_with("unverified: ")),
        "each item says whether it ran, and an item that did not run says why"
    );
}
