//! Real Unix device API over a PTY; this does not simulate UART electrical signals.
#![cfg(target_os = "linux")]
use shellune_serial::{CloseOutcome, SerialConfig, SerialEvent, SerialSession};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::time::{Duration, Instant};

#[test]
fn a_delayed_pty_peer_preserves_binary_data_and_closes_promptly() {
    let (mut master, mut slave) = (-1, -1);
    let mut name = [0i8; 128];
    // SAFETY: openpty initializes both owned descriptors and a bounded PTY name.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut peer = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap();
    let mut session =
        SerialSession::open(path, &SerialConfig::default()).expect("open real PTY slave");
    drop(slave);
    let payload: Vec<u8> = (0..=255).collect();
    session.send(&payload).expect("queue binary request");
    let expected = payload.clone();
    let worker = std::thread::spawn(move || {
        let mut request = vec![0; expected.len()];
        peer.read_exact(&mut request).unwrap();
        assert_eq!(request, expected);
        std::thread::sleep(Duration::from_secs(3));
        peer.write_all(&request).unwrap();
        peer
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut received = Vec::new();
    while received.len() < payload.len() && Instant::now() < deadline {
        for event in session.poll() {
            if let SerialEvent::Received(bytes) = event {
                received.extend(bytes);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(received, payload, "raw binary round trip through termios");
    let peer = worker.join().unwrap();
    let start = Instant::now();
    assert!(matches!(session.close(), CloseOutcome::Closed { .. }));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "idle read must be interrupted"
    );
    drop(peer);
}
