//! Opt-in real SSH flood probe. Uses a preverified fingerprint, never accepts an unknown key.
use shellune_core::{AuthMethod, ConnectionPolicy};
use shellune_ssh::{ConnectRequest, KnownHostKey, PtySize, RemoteEvent, RemoteSession};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() >= 4,
        "terminal_probe host port user [lines] [pause-seconds]"
    );
    let fingerprint =
        std::env::var("SHELLUNE_SSH_PROBE_FINGERPRINT").expect("preverified fingerprint");
    let key = std::env::var("SHELLUNE_SSH_PROBE_KEY").expect("key path");
    let lines: usize = args.get(4).map_or(200_000, |n| n.parse().unwrap());
    let pause: u64 = args.get(5).map_or(2, |n| n.parse().unwrap());
    let request = ConnectRequest::new(
        &args[1],
        args[2].parse().unwrap(),
        &args[3],
        AuthMethod::key_file(key, None),
        PtySize::new(120, 40),
        KnownHostKey::Trusted { fingerprint },
    )
    .with_connection_policy(
        ConnectionPolicy::default().with_connect_timeout(Duration::from_secs(180)),
    );
    let mut session = RemoteSession::connect(request);
    let start = Instant::now();
    let mut connected = false;
    let mut received = 0;
    let mut pending = Vec::new();
    let mut began = false;
    let mut completed = false;
    let mut total = 0usize;
    while start.elapsed() < Duration::from_secs(300) && !completed {
        for event in session.drain() {
            match event {
                RemoteEvent::Connected => {
                    connected = true;
                    println!("connected_seconds={:.3}", start.elapsed().as_secs_f64());
                    // Splitting the markers in the command keeps the shell's echo from
                    // being confused with output from the actual process.
                    let command = format!(
                        "stty -echo; python3 -u -c 'import os; os.write(1,b\"SH\"+b\"ELLUNE-BEGIN\\n\"); block=b\"log-line-0123456789abcdef\\n\"; [os.write(1,block*min(4096,{lines}-i)) for i in range(0,{lines},4096)]; os.write(1,b\"SH\"+b\"ELLUNE-END\\n\")'\n"
                    );
                    session.write(command.as_bytes()).expect("queue command");
                    std::thread::sleep(Duration::from_secs(pause));
                }
                RemoteEvent::Data(bytes) => {
                    total += bytes.len();
                    pending.extend_from_slice(&bytes);
                    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<_> = pending.drain(..=end).collect();
                        let text = String::from_utf8_lossy(&line);
                        if text.trim().ends_with("SHELLUNE-BEGIN") {
                            began = true;
                        } else if text.trim().ends_with("SHELLUNE-END") {
                            completed = true;
                        } else if began && !completed && text.trim() == "log-line-0123456789abcdef"
                        {
                            received += 1;
                        }
                    }
                    assert!(pending.len() < 65536, "unexpected unbounded line");
                }
                RemoteEvent::Failed(error) => panic!("SSH failed: {error}"),
                RemoteEvent::Exited { .. } => panic!("shell ended before the probe completed"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    session.close();
    assert!(
        connected && began && completed,
        "probe did not complete: connected={connected} began={began} completed={completed} received={received} total={total}"
    );
    assert_eq!(received, lines, "output lost or duplicated");
    println!(
        "PASS lines={received} elapsed_seconds={:.3} pause_seconds={pause}",
        start.elapsed().as_secs_f64()
    );
}
