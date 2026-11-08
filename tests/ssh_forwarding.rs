//! Local, remote and dynamic forwarding against a real OpenSSH server (R11).
//!
//! The checks that need a server say so and return when this host cannot start
//! one. A skip is not a pass. Traffic is a token written to a real listener and
//! read back, not a claim that a socket was asked for.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use shellune_core::{AuthMethod, ForwardId, ForwardKind, HostConfig};
use shellune_ssh::testserver::Harness;
use shellune_ssh::{
    ChainEvent, ChainRequest, ChainSession, ConnectRequest, ForwardCommand, ForwardEvent,
    ForwardFailure, ForwardLink, ForwardSpec, HopAuth, KnownHostKey, PtySize, RemoteEvent,
    RemoteSession,
};

const PATIENCE: Duration = Duration::from_secs(45);

fn ssh_gate() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn skip(what: &str) -> bool {
    if Harness::is_available() {
        return false;
    }
    eprintln!(
        "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so forwarding was not verified"
    );
    true
}

fn trusted(server: &Harness) -> KnownHostKey {
    KnownHostKey::Trusted {
        fingerprint: server.host_key_fingerprint(),
    }
}

fn password(server: &Harness) -> AuthMethod {
    AuthMethod::password(server.password())
}

fn dial(server: &Harness, shell: bool) -> (RemoteSession, ForwardLink) {
    let mut request = ConnectRequest::new(
        "127.0.0.1",
        server.port(),
        server.user(),
        password(server),
        PtySize::new(80, 24),
        trusted(server),
    );
    if !shell {
        request = request.without_shell();
    }
    let mut session = RemoteSession::connect(request);
    let link = session
        .take_forward_link()
        .expect("the connection exposes its forward link once");
    let deadline = Instant::now() + PATIENCE;
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::Connected => return (session, link),
                RemoteEvent::Failed(error) => panic!("connection failed: {error}"),
                RemoteEvent::HostKeyPrompt(_) => panic!("a trusted host must not ask again"),
                _ => {}
            }
        }
        if Instant::now() > deadline {
            panic!("the connection did not finish");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn spec(
    kind: ForwardKind,
    listen_port: u16,
    target_host: Option<&str>,
    target_port: Option<u16>,
) -> ForwardSpec {
    ForwardSpec {
        id: ForwardId::new(),
        generation: 1,
        kind,
        listen_host: "127.0.0.1".to_owned(),
        listen_port,
        target_host: target_host.map(str::to_owned),
        target_port,
    }
}

fn wait_settled(link: &mut ForwardLink) -> ForwardEvent {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for event in link.drain() {
            match event {
                ForwardEvent::AttemptFailed { .. } => {}
                other => return other,
            }
        }
        if !link.events_open() {
            return ForwardEvent::CarrierLost;
        }
        if Instant::now() > deadline {
            panic!("no forward result arrived");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn expect_listening(link: &mut ForwardLink, id: ForwardId) -> u16 {
    match wait_settled(link) {
        ForwardEvent::Listening {
            id: event_id, port, ..
        } => {
            assert_eq!(event_id, id);
            port
        }
        other => panic!("expected a real listener, got {other:?}"),
    }
}

fn start_wsl_echo(server: &Harness) -> u16 {
    let listed = server
        .as_user(
            "python3 -c 'import socket; s=socket.socket(); s.bind((\"127.0.0.1\", 0)); print(s.getsockname()[1])'",
        )
        .expect("a free port inside the server");
    let port: u16 = listed
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("echo port from {listed:?}"));
    let bind = format!("s.bind(('127.0.0.1', {port}))");
    let py = [
        "import socket",
        "s = socket.socket()",
        "s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)",
        bind.as_str(),
        "s.listen(16)",
        "while True:",
        "    c, _ = s.accept()",
        "    data = b''",
        "    while b'\\n' not in data:",
        "        chunk = c.recv(4096)",
        "        if not chunk:",
        "            break",
        "        data += chunk",
        "    c.sendall(data)",
        "    c.close()",
    ]
    .join("\n");
    let script = format!(
        "cat > /tmp/shellune-echo-{port}.py <<'PY'\n{py}\nPY\nnohup python3 /tmp/shellune-echo-{port}.py >/tmp/shellune-echo-{port}.log 2>&1 &\nfor _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", {port}), 1).close()' && exit 0; sleep 0.2; done\necho echo-log; cat /tmp/shellune-echo-{port}.log; exit 1\n"
    );
    server
        .as_user(&script)
        .unwrap_or_else(|error| panic!("the echo server did not start: {error}"));
    port
}

fn local_echo(port: u16, token: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("the listener accepts");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream.write_all(token).expect("write");
    let mut received = vec![0u8; token.len()];
    stream.read_exact(&mut received).expect("echo");
    received
}

fn refuse(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(200),
        )
        .is_err()
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("port {port} still accepts after the forward stopped");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn windows_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("echo port");
    let port = listener.local_addr().expect("addr").port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            thread::spawn(move || {
                let mut data = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match stream.read(&mut byte) {
                        Ok(1) => {
                            data.push(byte[0]);
                            if byte[0] == b'\n' {
                                break;
                            }
                        }
                        _ => return,
                    }
                }
                let _ = stream.write_all(&data);
            });
        }
    });
    port
}

fn wsl_roundtrip(server: &Harness, port: u16, token: &str) -> String {
    server
        .as_user(&format!(
            "python3 -c 'import socket; s=socket.create_connection((\"127.0.0.1\", {port}), 8); s.sendall(b\"{token}\\n\"); print(s.recv(256).decode(), end=\"\")'"
        ))
        .unwrap_or_else(|error| panic!("remote client failed: {error}"))
}

fn socks5(port: u16, host: &str, target: u16, token: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("socks");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream.write_all(&[5, 1, 0]).expect("methods");
    let mut method = [0u8; 2];
    stream.read_exact(&mut method).expect("method");
    assert_eq!(method, [5, 0], "no-auth");
    let mut request = vec![5, 1, 0, 3, u8::try_from(host.len()).expect("host")];
    request.extend(host.as_bytes());
    request.extend(target.to_be_bytes());
    stream.write_all(&request).expect("connect");
    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).expect("reply");
    assert_eq!(reply[1], 0, "socks connect failed: {reply:?}");
    stream.write_all(token).expect("payload");
    let mut received = vec![0u8; token.len()];
    stream.read_exact(&mut received).expect("socks echo");
    received
}

fn shell_still_answers(session: &mut RemoteSession) {
    session
        .write(b"printf 'FORWARD-SHELL-OK\\n'\n")
        .expect("the shell is still there");
    let deadline = Instant::now() + PATIENCE;
    let mut text = String::new();
    while Instant::now() < deadline {
        for event in session.drain() {
            if let RemoteEvent::Data(bytes) = event {
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        if text.contains("FORWARD-SHELL-OK") {
            return;
        }
        thread::sleep(Duration::from_millis(30));
    }
    panic!("the shell did not answer after the forward failure: {text:?}");
}

#[test]
fn local_remote_and_dynamic_forward_real_traffic_until_stopped_or_disconnected() {
    if skip("three forward kinds") {
        return;
    }
    let _gate = ssh_gate();
    let server = Harness::start("fwd-kinds", Harness::free_port()).expect("server");
    let echo = start_wsl_echo(&server);
    let (mut session, mut link) = dial(&server, false);

    let local = spec(
        ForwardKind::Local,
        Harness::free_port(),
        Some("127.0.0.1"),
        Some(echo),
    );
    let local_id = local.id;
    assert!(link.send(ForwardCommand::Start(local)));
    let local_port = expect_listening(&mut link, local_id);
    let token = b"local-token\n";
    assert_eq!(local_echo(local_port, token), token);
    assert!(link.send(ForwardCommand::Stop { id: local_id }));
    assert!(matches!(
        wait_settled(&mut link),
        ForwardEvent::Stopped { id, .. } if id == local_id
    ));
    refuse(local_port);

    let windows = windows_echo();
    let remote = spec(
        ForwardKind::Remote,
        {
            let listed = server
                .as_user(
                    "python3 -c 'import socket; s=socket.socket(); s.bind((\"127.0.0.1\", 0)); print(s.getsockname()[1])'",
                )
                .expect("remote port");
            listed
                .trim()
                .lines()
                .last()
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        },
        Some("127.0.0.1"),
        Some(windows),
    );
    let remote_id = remote.id;
    let remote_listen = remote.listen_port;
    assert!(link.send(ForwardCommand::Start(remote)));
    let granted = expect_listening(&mut link, remote_id);
    assert_eq!(granted, remote_listen);
    let remote_token = "remote-token";
    assert_eq!(
        wsl_roundtrip(&server, granted, remote_token).trim(),
        remote_token
    );
    assert!(link.send(ForwardCommand::Stop { id: remote_id }));
    assert!(matches!(
        wait_settled(&mut link),
        ForwardEvent::Stopped { id, .. } if id == remote_id
    ));

    let dynamic = spec(ForwardKind::Dynamic, Harness::free_port(), None, None);
    let dynamic_id = dynamic.id;
    assert!(link.send(ForwardCommand::Start(dynamic)));
    let socks = expect_listening(&mut link, dynamic_id);
    assert_eq!(
        socks5(socks, "127.0.0.1", echo, b"socks-token\n"),
        b"socks-token\n"
    );
    assert!(link.send(ForwardCommand::Stop { id: dynamic_id }));
    assert!(matches!(
        wait_settled(&mut link),
        ForwardEvent::Stopped { .. }
    ));
    refuse(socks);

    let again = spec(
        ForwardKind::Local,
        Harness::free_port(),
        Some("127.0.0.1"),
        Some(echo),
    );
    let again_id = again.id;
    assert!(link.send(ForwardCommand::Start(again)));
    let again_port = expect_listening(&mut link, again_id);
    assert_eq!(local_echo(again_port, b"again\n"), b"again\n");
    session.close();
    let lost = wait_settled(&mut link);
    assert!(
        matches!(
            lost,
            ForwardEvent::Failed {
                failure: ForwardFailure::CarrierLost,
                ..
            } | ForwardEvent::CarrierLost
        ),
        "disconnect must clear the listener, got {lost:?}"
    );
    refuse(again_port);
}

#[test]
fn bind_refusal_and_a_dead_target_keep_the_reason_and_the_shell() {
    if skip("forward failures") {
        return;
    }
    let _gate = ssh_gate();
    let server = Harness::start("fwd-fail", Harness::free_port()).expect("server");
    let (mut session, mut link) = dial(&server, true);

    let occupied = TcpListener::bind("127.0.0.1:0").expect("occupied");
    let occupied_port = occupied.local_addr().expect("addr").port();
    let blocked = spec(
        ForwardKind::Local,
        occupied_port,
        Some("127.0.0.1"),
        Some(9),
    );
    assert!(link.send(ForwardCommand::Start(blocked)));
    match wait_settled(&mut link) {
        ForwardEvent::Failed {
            failure: ForwardFailure::PortInUse { port, .. },
            ..
        } => assert_eq!(port, occupied_port),
        other => panic!("expected the port to be in use, got {other:?}"),
    }
    shell_still_answers(&mut session);

    let missing = spec(
        ForwardKind::Local,
        Harness::free_port(),
        Some("127.0.0.1"),
        Some(1),
    );
    assert!(link.send(ForwardCommand::Start(missing)));
    match wait_settled(&mut link) {
        ForwardEvent::Failed {
            failure: ForwardFailure::TargetUnreachable { port, .. },
            ..
        } => assert_eq!(port, 1),
        other => panic!("expected the target to be unreachable, got {other:?}"),
    }
    shell_still_answers(&mut session);
    drop(occupied);
}

#[test]
fn a_server_that_disables_forwarding_is_named_and_the_shell_stays() {
    if skip("forwarding disabled") {
        return;
    }
    let _gate = ssh_gate();
    let server =
        Harness::start_without_forwarding("fwd-deny", Harness::free_port()).expect("server");
    let (mut session, mut link) = dial(&server, true);
    let rule = spec(
        ForwardKind::Local,
        Harness::free_port(),
        Some("127.0.0.1"),
        Some(9),
    );
    assert!(link.send(ForwardCommand::Start(rule)));
    match wait_settled(&mut link) {
        ForwardEvent::Failed {
            failure: ForwardFailure::ServerRefused { detail },
            ..
        } => {
            let text = detail.to_ascii_lowercase();
            assert!(
                text.contains("administratively")
                    || text.contains("denied")
                    || text.contains("refus"),
                "{detail}"
            );
        }
        other => panic!("expected the server to refuse forwarding, got {other:?}"),
    }
    shell_still_answers(&mut session);
}

#[test]
fn a_jump_carries_the_forward_to_the_target_and_dies_with_the_jump() {
    if skip("jump forward") {
        return;
    }
    let _gate = ssh_gate();
    let mut jump = Harness::start("fwd-jump", Harness::free_port()).expect("jump");
    let target = Harness::start("fwd-target", Harness::free_port()).expect("target");
    let echo = start_wsl_echo(&target);
    let jump_host = HostConfig::new("jump", "127.0.0.1", jump.port(), jump.user()).expect("host");
    let mut target_host =
        HostConfig::new("target", "127.0.0.1", target.port(), target.user()).expect("host");
    target_host.set_jumps([jump_host.id()]);
    let mut session = ChainSession::connect(
        ChainRequest::new(
            vec![jump_host.clone(), target_host.clone()],
            target_host.id(),
            vec![
                HopAuth::new(jump_host.id(), password(&jump), trusted(&jump)),
                HopAuth::new(target_host.id(), password(&target), trusted(&target)),
            ],
            PtySize::new(80, 24),
        )
        .with_connect_timeout(Duration::from_secs(20))
        .without_shell(),
    );
    let mut link = session
        .take_forward_link()
        .expect("the chain exposes the target forward link");
    let deadline = Instant::now() + PATIENCE;
    loop {
        match session.try_event() {
            Some(ChainEvent::Connected { hop }) => {
                assert_eq!(hop.host_id, target_host.id());
                break;
            }
            Some(ChainEvent::Failed(error)) => panic!("chain failed: {error}"),
            Some(_) => {}
            None if Instant::now() > deadline => panic!("the chain did not connect"),
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
    let rule = spec(
        ForwardKind::Local,
        Harness::free_port(),
        Some("127.0.0.1"),
        Some(echo),
    );
    let id = rule.id;
    assert!(link.send(ForwardCommand::Start(rule)));
    let port = expect_listening(&mut link, id);
    assert_eq!(local_echo(port, b"via-jump\n"), b"via-jump\n");
    jump.stop();
    let lost = wait_settled(&mut link);
    assert!(
        matches!(
            lost,
            ForwardEvent::Failed {
                failure: ForwardFailure::CarrierLost,
                ..
            } | ForwardEvent::CarrierLost
        ),
        "a dead jump must drop the forward, got {lost:?}"
    );
    refuse(port);
}
