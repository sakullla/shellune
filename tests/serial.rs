//! The serial backend, end to end (R17, R21).
//!
//! Two kinds of test live here, and the names say which is which:
//!
//! * the tests that call the real backend - the port list and opening a port that is not
//!   there - run against the operating system and prove the system API is reached and its
//!   answers are translated into a reason;
//! * the tests that drive [`shellune_serial::testing`] run a real [`SerialSession`] - its
//!   reader thread, writer thread, bounded queue, cancellation and close - over an
//!   in-memory port. They prove this crate's behaviour with bytes, bounds and a driver
//!   that stops answering. They are not evidence about a UART, a cable or a virtual
//!   com0com pair, and nothing here is written as if they were.

use std::time::{Duration, Instant};

use shellune_serial::testing::{FakeSpec, fake, session_over, session_over_with, session_pair};
use shellune_serial::{
    CloseOutcome, DataBits, FlowControl, Parity, RawCodeKind, RawError, SerialCause, SerialConfig,
    SerialError, SerialEvent, SerialSession, SessionLimits, SessionState, StopBits, list_ports,
};

/// Polls a session until it has delivered `want` bytes, or the timeout runs out.
fn collect_bytes(session: &mut SerialSession, want: usize, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut collected = Vec::new();
    while collected.len() < want && Instant::now() < deadline {
        for event in session.poll() {
            if let SerialEvent::Received(data) = event {
                collected.extend_from_slice(&data);
            }
        }
        if collected.len() < want {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    collected
}

/// Polls a session until a predicate holds for one of its events.
fn poll_until(
    session: &mut SerialSession,
    timeout: Duration,
    mut matches: impl FnMut(&SerialEvent) -> bool,
) -> Option<SerialEvent> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in session.poll() {
            if matches(&event) {
                return Some(event);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

#[test]
fn the_port_list_comes_from_the_system() {
    // This is the real enumeration: the Windows device map, or /dev on Unix. A machine
    // without serial ports answers with an empty list, which is not an error.
    let ports = list_ports().expect("the port list must be readable on this machine");
    for port in &ports {
        assert!(!port.name().is_empty(), "{port:?}");
        assert!(port.path().contains(port.name()), "{port:?}");
    }
}

#[test]
fn a_port_that_does_not_exist_is_reported_as_missing() {
    // COM255 exists on no machine, and /dev/COM255 exists on no Unix either, so the same
    // name asks the same question on both backends: open a port that is not there.
    let error = SerialSession::open("COM255", &SerialConfig::default())
        .expect_err("a port that does not exist must not open");
    assert!(
        error.is(SerialCause::NotFound),
        "expected a missing port, got {error}"
    );
    assert_eq!(error.port(), Some("COM255"));
    let raw = error.raw().expect("the system code is kept");
    assert!(raw.code() > 0, "{raw}");
    assert!(!raw.text().is_empty(), "{raw}");
}

#[test]
fn an_impossible_setting_is_refused_before_the_device_is_touched() {
    let config = SerialConfig::default().with_baud(0);
    let error =
        SerialSession::open("COM255", &config).expect_err("a zero baud rate must be refused");
    assert_eq!(error.cause(), SerialCause::UnsupportedConfig);
    // No port in the error means the device was never opened, which is the point of
    // validating first: a wrong setting must not look like a driver failure.
    assert_eq!(error.port(), None, "{error}");

    let five_two = SerialConfig::new(
        9600,
        DataBits::Five,
        Parity::None,
        StopBits::Two,
        FlowControl::None,
    );
    let error = SerialSession::open("COM255", &five_two).expect_err("5 data bits + 2 stop");
    assert_eq!(error.cause(), SerialCause::UnsupportedConfig);
}

#[test]
fn a_virtual_pair_carries_ascii_utf8_nul_and_invalid_utf8_unchanged() {
    // Non-default settings on purpose: 7E2 with hardware flow control is nothing like the
    // default, and the bytes must not depend on what the settings are.
    let config = SerialConfig::new(
        57_600,
        DataBits::Seven,
        Parity::Even,
        StopBits::Two,
        FlowControl::Hardware,
    );
    let (mut left, mut right) = session_pair(FakeSpec::new(), FakeSpec::new(), config);

    let payload = [
        b"AT+VER?\r\n".as_slice(),
        "温度 23.5\u{00b0}C\n".as_bytes(),
        &[0x00],
        &[0xff, 0xfe, 0xc3, 0x28],
    ]
    .concat();
    left.send(&payload).expect("the queue is empty");
    let received = collect_bytes(&mut right, payload.len(), Duration::from_secs(5));
    assert_eq!(received, payload, "bytes must survive the pair unchanged");

    // The other direction too, so neither end is special.
    let back = [0x00u8, 0x41, 0x80, 0x0d];
    right.send(&back).expect("the queue is empty");
    let returned = collect_bytes(&mut left, back.len(), Duration::from_secs(5));
    assert_eq!(returned, back);
}

#[test]
fn a_burst_that_arrives_faster_than_the_caller_polls_does_not_lose_bytes() {
    // The receive side is bounded too: once too many bytes wait for a poll, the reader
    // stops reading the device instead of growing a buffer or dropping bytes.
    let limits = SessionLimits {
        queued_frames: 8,
        queued_bytes: 4096,
        pending_rx_bytes: 1024,
        close_timeout: Duration::from_millis(500),
    };
    let port = fake("fake-a", FakeSpec::new());
    let mut session = session_over_with(port.clone(), SerialConfig::default(), limits);

    let payload: Vec<u8> = (0..64 * 1024).map(|index| (index % 251) as u8).collect();
    for chunk in payload.chunks(512) {
        port.push_from_device(chunk);
    }
    // Nothing is polled here on purpose: this is the congestion the limit exists for.
    std::thread::sleep(Duration::from_millis(50));

    let received = collect_bytes(&mut session, payload.len(), Duration::from_secs(10));
    assert_eq!(received.len(), payload.len(), "no byte may be lost");
    assert_eq!(received, payload);
}

#[test]
fn a_full_send_queue_refuses_bytes_instead_of_dropping_them() {
    // The driver never accepts a write, so the queue fills up and stays full.
    let limits = SessionLimits {
        queued_frames: 3,
        queued_bytes: 24,
        pending_rx_bytes: 4096,
        close_timeout: Duration::from_millis(500),
    };
    let port = fake("fake-a", FakeSpec::new().holding_writes());
    let mut session = session_over_with(port.clone(), SerialConfig::default(), limits);

    let frame = [b'x'; 8];
    let mut accepted = 0usize;
    let mut refusal = None;
    for _ in 0..6 {
        match session.send(&frame) {
            Ok(_) => accepted += 1,
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }
    let refusal = refusal.expect("the queue must reach its limit and refuse");
    assert_eq!(refusal.cause(), SerialCause::QueueFull);
    assert!(
        refusal.detail().unwrap_or_default().contains("at most 3"),
        "{refusal}"
    );
    assert!((1..6).contains(&accepted), "accepted {accepted}");

    // Everything the queue accepted is still there: nothing was thrown away to make room.
    assert_eq!(session.queued_bytes(), accepted * frame.len());
    assert!(session.pending_frames() <= accepted);
    assert!(session.pending_frames() + 1 >= accepted);
    assert!(port.written().is_empty(), "the driver accepted nothing");

    let _ = session.close();
}

#[test]
fn a_frame_the_driver_takes_in_pieces_is_reported_and_not_replayed() {
    // The driver accepts three bytes and then stops accepting: the close lands inside the
    // frame, which is the case "a partial write must not be replayed" is about.
    let limits = SessionLimits {
        queued_frames: 8,
        queued_bytes: 1024,
        pending_rx_bytes: 1024,
        close_timeout: Duration::from_millis(500),
    };
    let port = fake(
        "fake-a",
        FakeSpec::new().with_write_chunk(3).stalling_after(1),
    );
    let mut session = session_over_with(port.clone(), SerialConfig::default(), limits);

    let payload: Vec<u8> = (0..9u8).map(|byte| b'A' + byte).collect();
    let frame = session.send(&payload).expect("the queue is empty");

    let first = poll_until(&mut session, Duration::from_secs(2), |event| {
        matches!(event, SerialEvent::Sent { .. })
    });
    let Some(SerialEvent::Sent {
        frame: sent_frame,
        bytes,
    }) = first
    else {
        panic!("the driver accepted the first piece");
    };
    assert_eq!(sent_frame, frame);
    assert_eq!(bytes, 3);

    let outcome = session.close();
    assert!(
        matches!(outcome, CloseOutcome::Closed { .. }),
        "{outcome:?}"
    );

    let mut incomplete = None;
    for event in session.poll() {
        if let SerialEvent::SendIncomplete {
            frame,
            sent,
            requested,
        } = event
        {
            assert_eq!(frame, 0);
            incomplete = Some((sent, requested));
        }
    }
    let (sent, requested) = incomplete.expect("the stopped frame must be reported");
    assert_eq!((sent, requested), (3, payload.len()));

    // The port received exactly the prefix once: not the frame again from the top, and
    // never a byte twice, which is what replaying a partial write would look like.
    assert_eq!(port.written(), payload[..3].to_vec());
}

#[test]
fn an_empty_send_is_refused_instead_of_queued() {
    let (port, _peer) = shellune_serial::testing::fake_pair();
    let session = session_over(port, SerialConfig::default());
    let error = session
        .send(&[])
        .expect_err("an empty frame is not a frame");
    assert_eq!(error.cause(), SerialCause::Refused);
    assert_eq!(session.pending_frames(), 0);
}

#[test]
fn closing_cancels_a_blocked_read_and_releases_the_port() {
    let port = fake("fake-a", FakeSpec::new());
    let mut session = session_over(port.clone(), SerialConfig::default());
    assert_eq!(session.state(), SessionState::Open);
    std::thread::sleep(Duration::from_millis(20));

    let outcome = session.close();
    assert!(
        matches!(outcome, CloseOutcome::Closed { .. }),
        "{outcome:?}"
    );
    assert_eq!(session.state(), SessionState::Closed);
    assert!(port.is_released(), "the port must be released");
    assert!(port.interrupts() >= 1, "a close must cancel the wait");
    assert_eq!(session.close(), CloseOutcome::AlreadyClosed);
    assert_eq!(session.send(b"x").unwrap_err().cause(), SerialCause::Closed);
}

#[test]
fn a_driver_that_never_answers_keeps_the_port_in_closing() {
    // The recipe's fourth outcome: a driver that does not complete the cancelled
    // operation must not be reported as released, and the port must not be reused.
    let limits = SessionLimits {
        close_timeout: Duration::from_millis(120),
        ..SessionLimits::default()
    };
    let port = fake("fake-a", FakeSpec::new().ignoring_interrupt());
    let mut session = session_over_with(port.clone(), SerialConfig::default(), limits);
    // The read has to be in flight for this test to be about a stuck driver, so the
    // reader thread is given the moment it needs to reach its first read.
    std::thread::sleep(Duration::from_millis(30));

    let outcome = session.close();
    assert!(
        matches!(outcome, CloseOutcome::StillClosing { .. }),
        "a stuck driver must not be reported as released: {outcome:?}"
    );
    assert_eq!(session.state(), SessionState::Closing);
    assert!(!port.is_released(), "the port is still owned");

    port.release_driver();
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = loop {
        let outcome = session.close();
        if !matches!(outcome, CloseOutcome::StillClosing { .. }) || Instant::now() >= deadline {
            break outcome;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        matches!(outcome, CloseOutcome::Closed { .. }),
        "{outcome:?}"
    );
    assert_eq!(session.state(), SessionState::Closed);
    assert!(port.is_released());
}

#[test]
fn an_unplugged_device_is_reported_as_a_disconnect_and_the_port_is_released() {
    let failure = SerialError::new(SerialCause::Disconnected)
        .at("fake-a")
        .with_raw(RawError::new(RawCodeKind::Win32, 1167));
    let port = fake("fake-a", FakeSpec::new().failing_reads(failure.clone()));
    let mut session = session_over(port.clone(), SerialConfig::default());

    let event = poll_until(&mut session, Duration::from_secs(2), |event| {
        matches!(event, SerialEvent::Disconnected { .. })
    });
    let Some(SerialEvent::Disconnected { error }) = event else {
        panic!("a failing read must be reported as a disconnect");
    };
    assert_eq!(error.cause(), SerialCause::Disconnected);
    assert_eq!(error.raw().map(RawError::code), Some(1167));

    // The session settles into its final state and lets the port go.
    let _ = session.poll();
    assert_eq!(session.state(), SessionState::Failed);
    assert!(port.is_released());
    assert_eq!(
        session.send(b"x").unwrap_err().cause(),
        SerialCause::Disconnected
    );
}
