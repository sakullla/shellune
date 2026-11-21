//! Connects to a real RDP server with this crate's own client and reports what it
//! sees, so the protocol work can be checked against a deployable endpoint.
//!
//! RDP_HOST, RDP_PORT, RDP_USER, RDP_PASSWORD override the defaults; RDP_SECONDS
//! bounds the run. Exit status 0 means a picture arrived.
use shellune_desktop::{DesktopClient, Event, PointerButtons, Protocol, State};
use std::time::{Duration, Instant};

fn main() {
    let host = std::env::var("RDP_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = std::env::var("RDP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3389u16);
    let user = std::env::var("RDP_USER").unwrap_or_else(|_| "shellune-x11-real".to_owned());
    let password =
        std::env::var("RDP_PASSWORD").unwrap_or_else(|_| "shellune-test-password".to_owned());
    let seconds = std::env::var("RDP_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30u64);

    let mut client = DesktopClient::new(Protocol::Rdp).expect("client");
    client.set_server(&host, port).expect("server");
    client
        .set_credentials(Some(&user), Some(&password), None)
        .expect("credentials");
    client.set_size(800, 600).expect("size");
    client.set_clipboard(true).expect("clipboard");
    client.connect().expect("connect");

    // A frame dump makes the picture reviewable: without it the probe can only say
    // that pixels arrived, not that they are the desktop.
    let dump = std::env::var("RDP_PPM").ok();
    let mut desktop: Vec<u8> = Vec::new();
    let mut desktop_size = (0u32, 0u32);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames = 0usize;
    let mut picture = false;
    let start = Instant::now();
    let mut clicked: Option<Instant> = None;
    let mut after_click = 0usize;
    let click = std::env::var("RDP_CLICK").unwrap_or_else(|_| "12,12,l".to_owned());
    let click_delay = std::env::var("RDP_DELAY")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(3));
    let clip_done = false;
    let mut key_sent = false;
    let mut clipboard_pushed: Option<Duration> = None;
    let mut terminal_opened: Option<Duration> = None;
    let mut pasted: Option<Duration> = None;
    let want_input = std::env::var_os("RDP_INPUT").is_some();
    while Instant::now() < deadline {
        if std::env::var_os("RDP_CLIPTEST").is_some() && !clip_done {
            let elapsed = start.elapsed();
            if clipboard_pushed.is_none() && elapsed >= Duration::from_secs(2) {
                let entry = shellune_desktop::ClipboardEntry::text("shellune-clip-42");
                println!("push clipboard {:?}", client.push_clipboard(&[entry]));
                clipboard_pushed = Some(elapsed);
            } else if terminal_opened.is_none() && elapsed >= Duration::from_secs(4) {
                let keys = [(0x1d, false), (0x38, false), (0x14, false)];
                for (code, extended) in keys {
                    let _ = client.key(code, 0, true, extended);
                }
                for (code, extended) in keys.iter().rev() {
                    let _ = client.key(*code, 0, false, *extended);
                }
                println!("opened terminal");
                terminal_opened = Some(elapsed);
            } else if pasted.is_none() && elapsed >= Duration::from_secs(9) {
                let keys = [(0x1d, false), (0x2a, false), (0x2f, false)];
                for (code, extended) in keys {
                    let _ = client.key(code, 0, true, extended);
                }
                for (code, extended) in keys.iter().rev() {
                    let _ = client.key(*code, 0, false, *extended);
                }
                println!("pasted");
                pasted = Some(elapsed);
            }
        }
        if std::env::var_os("RDP_KEYS").is_some()
            && !key_sent
            && start.elapsed() >= Duration::from_secs(7)
        {
            // Escape: closes whatever the click opened, so the keyboard is shown
            // to have reached the session too.
            let _ = client.key(0x01, 0, true, false);
            let _ = client.key(0x01, 0, false, false);
            println!("sent escape");
            key_sent = true;
        }
        if want_input && clicked.is_none() && start.elapsed() >= click_delay {
            let mut parts = click.split(',');
            let x = parts
                .next()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(12u16);
            let y = parts
                .next()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(12u16);
            let button = match parts.next().map(str::trim) {
                Some("r") => PointerButtons::RIGHT,
                Some("m") => PointerButtons::MIDDLE,
                _ => PointerButtons::LEFT,
            };
            let _ = client.pointer(x, y, button);
            let _ = client.pointer(x, y, PointerButtons(0));
            println!("click {x},{y} {button:?}");
            clicked = Some(Instant::now());
        }
        match client.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::State {
                state,
                failure,
                reason,
                ..
            }) => {
                println!("state {state:?} failure={failure:?} reason={reason}");
                if matches!(state, State::Failed(_) | State::Closed) {
                    break;
                }
            }
            Ok(Event::Certificate {
                request_id,
                subject,
                issuer,
                fingerprint,
                ..
            }) => {
                println!("certificate subject={subject:?} issuer={issuer:?} fp={fingerprint}");
                let accept = std::env::var_os("RDP_REJECT_CERT").is_none();
                client
                    .answer_certificate(request_id, accept)
                    .expect("answer certificate");
            }
            Ok(Event::Frame {
                x,
                y,
                width,
                height,
                desktop_width,
                desktop_height,
                stride,
                pixels,
            }) => {
                if dump.is_some() && desktop_width > 0 && desktop_height > 0 {
                    if desktop_size != (desktop_width, desktop_height) {
                        desktop_size = (desktop_width, desktop_height);
                        desktop = vec![255; desktop_width as usize * desktop_height as usize * 4];
                    }
                    let row = width as usize * 4;
                    for line in 0..height as usize {
                        let source = line * stride as usize;
                        let start =
                            (y as usize + line) * desktop_width as usize * 4 + x as usize * 4;
                        let end = start + row.min(pixels.len().saturating_sub(source));
                        if end <= desktop.len() && source + row <= pixels.len() {
                            desktop[start..end].copy_from_slice(&pixels[source..source + row]);
                        }
                    }
                }
                frames += 1;
                if let Some(at) = clicked
                    && at.elapsed() < Duration::from_secs(4)
                    && frames > 1
                {
                    after_click += 1;
                }
                if frames == 1 {
                    println!("first frame {width}x{height}");
                }
                picture = true;
            }
            Ok(Event::Clipboard { entries }) => {
                println!("clipboard entries={}", entries.len());
            }
            Ok(Event::Notice { notice }) => println!("notice {notice:?}"),
            Ok(Event::Pointer(_)) | Err(_) => {}
        }
    }
    let _ = client.cancel();
    let _ = client.wait_for_worker(Duration::from_secs(5));
    if let (Some(path), true) = (dump, !desktop.is_empty()) {
        let (width, height) = desktop_size;
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for pixel in desktop.as_chunks::<4>().0 {
            ppm.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        if std::fs::write(&path, ppm).is_ok() {
            println!("wrote {path}");
        }
    }
    if clicked.is_some() {
        println!("after click frames={after_click}");
    }
    println!("frames={frames} picture={picture}");
    if !picture {
        std::process::exit(1);
    }
}
