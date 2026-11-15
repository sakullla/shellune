//! Opt-in endpoint probe. Credentials are read from the environment, never logged.
use shellune_desktop::{DesktopClient, Event, PointerButtons, Protocol, State};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let protocol = match args.get(1).map(String::as_str) {
        Some("rdp") => Protocol::Rdp,
        Some("vnc") => Protocol::Vnc,
        _ => panic!("usage: probe rdp|vnc host port [timeout-seconds]"),
    };
    let mut client = DesktopClient::new(protocol).expect("backend");
    client
        .set_server(&args[2], args[3].parse().expect("port"))
        .expect("server");
    client.set_size(800, 600).expect("size");
    let password = std::env::var("SHELLUNE_PROBE_PASSWORD").ok();
    client
        .set_credentials(Some("shellune-probe"), password.as_deref(), None)
        .expect("credentials");
    let start = Instant::now();
    let timeout = Duration::from_secs(args.get(4).map_or(60, |s| s.parse().expect("timeout")));
    client.connect().expect("start");
    let mut connected = false;
    let mut frames = 0;
    let reject_certificate = std::env::var_os("SHELLUNE_PROBE_REJECT_CERTIFICATE").is_some();
    let mut asked_certificate = false;
    while start.elapsed() < timeout {
        match client.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::State {
                state,
                failure,
                reason,
                ..
            }) => {
                println!(
                    "{:.3}s {state:?} {failure:?} {reason}",
                    start.elapsed().as_secs_f64()
                );
                connected |= state == State::Connected;
                if matches!(state, State::Failed(_) | State::Closed) {
                    break;
                }
            }
            Ok(Event::Certificate { request_id, .. }) => {
                asked_certificate = true;
                // Only this explicitly invoked test probe trusts a test server.
                client
                    .answer_certificate(request_id, !reject_certificate)
                    .expect("test certificate");
            }
            Ok(Event::Frame {
                width,
                height,
                pixels,
                ..
            }) => {
                frames += 1;
                if frames == 1 {
                    println!(
                        "{:.3}s first frame {width}x{height}, {} bytes",
                        start.elapsed().as_secs_f64(),
                        pixels.len()
                    );
                }
            }
            _ => {}
        }
        if connected && frames > 0 {
            client.key(0x1e, 0x61, true, false).expect("key down");
            client.key(0x1e, 0x61, false, false).expect("key up");
            client.pointer(30, 30, PointerButtons(0)).expect("pointer");
            break;
        }
    }
    let closing = Instant::now();
    let _ = client.cancel();
    client
        .wait_for_worker(Duration::from_secs(5))
        .expect("worker stops");
    println!(
        "close={}ms, connected={connected}, frames={frames}",
        closing.elapsed().as_millis()
    );
    if reject_certificate {
        assert!(
            asked_certificate && !connected && frames == 0,
            "reject must prevent the session"
        );
    } else {
        assert!(
            connected && frames > 0,
            "endpoint must connect and deliver pixels"
        );
    }
}
