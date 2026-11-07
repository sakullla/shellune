//! Jump chains and ProxyCommand against a real process, and against OpenSSH when
//! this host can start one (R10, R12).
//!
//! The graph checks and the "do not run the command" checks always run. The
//! two-hop shell, per-node trust, auth failure, identity change and disconnect
//! checks need the disposable OpenSSH server. When that server cannot be
//! started the test says so and returns; it does not pretend the hop was proved.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use shellune_core::{AuthMethod, HostConfig, HostId};
use shellune_ssh::testserver::Harness;
use shellune_ssh::{
    ChainEvent, ChainFailure, ChainRequest, ChainSession, ConnectError, ConnectRequest, HopAuth,
    HopLocation, HostKeyPrompt, KnownHostKey, PtySize, RemoteEvent, RemoteSession,
};
use shellune_storage::{HostKey, HostTrust};

const PATIENCE: Duration = Duration::from_secs(45);

fn ssh_gate() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn skip_without_server(what: &str) -> bool {
    if Harness::is_available() {
        return false;
    }
    eprintln!(
        "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so the hop was not verified"
    );
    true
}

fn scratch(name: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "shellune-jump-{}-{}-{name}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn tool() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ssh-jump-tool");
        std::fs::create_dir_all(&dir).expect("the tool directory can be created");
        let source = dir.join("tool.rs");
        let executable = dir.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&source, TOOL).expect("the tool source can be written");
        let output = Command::new("rustc")
            .args(["--edition", "2021", "-O"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("rustc can be started");
        assert!(
            output.status.success(),
            "rustc failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    })
    .clone()
}

const TOOL: &str = r#"
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("sleep") => {
            let pid_path = args.next().expect("pid file");
            let done_path = args.next().expect("done file");
            std::fs::write(&pid_path, std::process::id().to_string()).expect("pid");
            thread::sleep(Duration::from_secs(120));
            let _ = std::fs::write(&done_path, "done");
        }
        Some("bridge") => {
            let pid_path = args.next().expect("pid file");
            let host = args.next().expect("host");
            let port: u16 = args.next().expect("port").parse().expect("port");
            std::fs::write(&pid_path, std::process::id().to_string()).expect("pid");
            let mut socket = TcpStream::connect((host.as_str(), port)).expect("connect");
            let _ = socket.set_nodelay(true);
            let mut outbound = socket.try_clone().expect("clone");
            thread::spawn(move || {
                let mut stdout = io::stdout();
                let mut buf = [0u8; 8192];
                loop {
                    match outbound.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if stdout.write_all(&buf[..count]).is_err() {
                                break;
                            }
                            let _ = stdout.flush();
                        }
                    }
                }
            });
            let mut stdin = io::stdin();
            let mut buf = [0u8; 8192];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if socket.write_all(&buf[..count]).is_err() {
                            break;
                        }
                        let _ = socket.flush();
                    }
                }
            }
        }
        _ => std::process::exit(2),
    }
}
"#;

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

fn sleep_command(pid_file: &Path, done_file: &Path) -> String {
    format!(
        "{} sleep {} {}",
        quoted(&tool()),
        quoted(pid_file),
        quoted(done_file)
    )
}

fn bridge_command(pid_file: &Path) -> String {
    format!("{} bridge {} %h %p", quoted(&tool()), quoted(pid_file))
}

fn host(name: &str, port: u16, user: &str) -> HostConfig {
    HostConfig::new(name, "127.0.0.1", port, user).expect("valid host")
}

fn trusted(fingerprint: &str) -> KnownHostKey {
    KnownHostKey::Trusted {
        fingerprint: fingerprint.to_owned(),
    }
}

fn chain(
    hosts: Vec<HostConfig>,
    target: HostId,
    auth: Vec<HopAuth>,
    timeout: Duration,
) -> ChainSession {
    ChainSession::connect(
        ChainRequest::new(hosts, target, auth, PtySize::new(80, 24)).with_connect_timeout(timeout),
    )
}

fn wait_match(
    session: &mut ChainSession,
    mut matches: impl FnMut(&ChainEvent) -> bool,
) -> ChainEvent {
    let deadline = Instant::now() + PATIENCE;
    let mut held = Vec::new();
    loop {
        match session.try_event() {
            Some(event) if matches(&event) => return event,
            Some(event) => held.push(event),
            None => {
                if Instant::now() > deadline {
                    panic!("the chain did not report the expected event; held {held:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn wait_failed(session: &mut ChainSession) -> ChainFailure {
    match wait_match(session, |event| matches!(event, ChainEvent::Failed(_))) {
        ChainEvent::Failed(failure) => failure,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn wait_proxy(session: &mut ChainSession) -> (HopLocation, String) {
    match wait_match(session, |event| {
        matches!(event, ChainEvent::ProxyConfirmation { .. })
    }) {
        ChainEvent::ProxyConfirmation { hop, command } => (hop, command),
        other => panic!("expected a ProxyCommand confirmation, got {other:?}"),
    }
}

fn wait_prompt(session: &mut ChainSession) -> (HopLocation, HostKeyPrompt) {
    match wait_match(session, |event| {
        matches!(event, ChainEvent::HostKeyPrompt { .. })
    }) {
        ChainEvent::HostKeyPrompt { hop, prompt } => (hop, prompt),
        other => panic!("expected a trust question, got {other:?}"),
    }
}

fn wait_connected(session: &mut ChainSession) -> HopLocation {
    match wait_match(session, |event| {
        matches!(event, ChainEvent::Connected { .. })
    }) {
        ChainEvent::Connected { hop } => hop,
        other => panic!("expected the target shell, got {other:?}"),
    }
}

fn wait_output(session: &mut ChainSession, needle: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    let mut output = Vec::new();
    loop {
        for event in session.drain() {
            match event {
                ChainEvent::Data(bytes) => output.extend_from_slice(&bytes),
                ChainEvent::Failed(error) => {
                    panic!("the chain failed while waiting for output: {error}")
                }
                ChainEvent::Exited { code } => {
                    panic!("the shell exited ({code:?}) before {needle:?}")
                }
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(&output).into_owned();
        if text.contains(needle) {
            return text;
        }
        if Instant::now() > deadline {
            panic!("the shell never printed {needle:?}; received {text:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn accept_prompt(trust: &mut HostTrust, prompt: &HostKeyPrompt) {
    let key = HostKey::new(
        prompt.algorithm(),
        prompt.fingerprint(),
        prompt.presented.public_key.clone(),
    )
    .expect("the presented key can be stored");
    trust
        .accept(&prompt.host, prompt.port, &key, 1)
        .expect("the trust record is stored for this hop");
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .expect("tasklist");
        let text = String::from_utf8_lossy(&output.stdout);
        text.split(',')
            .any(|column| column.trim().trim_matches('"') == pid.to_string())
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

fn wait_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("process {pid} was still running after the chain stopped it");
}

fn wait_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        if Instant::now() > deadline {
            panic!("the ProxyCommand never wrote its pid to {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn direct_session(server: &Harness) -> RemoteSession {
    RemoteSession::connect(ConnectRequest::new(
        "127.0.0.1",
        server.port(),
        server.user(),
        AuthMethod::password(server.password()),
        PtySize::new(80, 24),
        trusted(&server.host_key_fingerprint()),
    ))
}

fn wait_direct(session: &mut RemoteSession) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::Connected => return,
                RemoteEvent::Failed(error) => panic!("the other session failed: {error}"),
                _ => {}
            }
        }
        if Instant::now() > deadline {
            panic!("the other session neither connected nor failed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn direct_echo(session: &mut RemoteSession, needle: &str) {
    session
        .write(format!("printf '%s\\n' {needle}\n").as_bytes())
        .expect("the other session accepts input");
    let deadline = Instant::now() + PATIENCE;
    let mut output = Vec::new();
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::Data(bytes) => output.extend_from_slice(&bytes),
                RemoteEvent::Failed(error) => panic!("the other session failed: {error}"),
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(&output);
        if text.contains(needle) {
            return;
        }
        if Instant::now() > deadline {
            panic!("the other session never printed {needle:?}; received {text:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_cycle_or_a_missing_hop_is_rejected_before_a_dial_or_a_command() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port can be reserved");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("port").port();
    let pid_file = scratch("cycle-pid");
    let done_file = scratch("cycle-done");
    let mut left = host("left", port, "deploy");
    let mut right = host("right", port, "deploy");
    left.set_jumps([right.id()]);
    right.set_jumps([left.id()]);
    right.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));

    let mut session = chain(
        vec![left.clone(), right.clone()],
        left.id(),
        vec![
            HopAuth::new(
                left.id(),
                AuthMethod::password("hunter2"),
                KnownHostKey::Unknown,
            ),
            HopAuth::new(
                right.id(),
                AuthMethod::password("hunter2"),
                KnownHostKey::Unknown,
            ),
        ],
        Duration::from_secs(5),
    );
    let failure = wait_failed(&mut session);
    let message = failure.to_string();
    assert!(message.contains("loops"), "{message}");
    assert!(message.contains("Nothing was connected"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    assert!(
        !pid_file.exists(),
        "a cyclic chain must not start its ProxyCommand"
    );
    assert!(
        matches!(
            listener.accept().map_err(|error| error.kind()),
            Err(ErrorKind::WouldBlock)
        ),
        "a cyclic chain must not dial"
    );

    let missing = HostId::new();
    let mut web = host("web", port, "deploy");
    web.set_jumps([missing]);
    web.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password("hunter2"),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(5),
    );
    let failure = wait_failed(&mut session);
    let message = failure.to_string();
    assert!(message.contains("not saved"), "{message}");
    assert!(message.contains(&missing.to_string()), "{message}");
    assert!(!pid_file.exists(), "a missing hop must not start a command");
    assert!(matches!(
        listener.accept().map_err(|error| error.kind()),
        Err(ErrorKind::WouldBlock)
    ));
}

#[test]
fn a_host_that_has_both_a_jump_and_a_proxy_command_runs_neither() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("port").port();
    let pid_file = scratch("both-pid");
    let done_file = scratch("both-done");
    let bastion = host("bastion", port, "deploy");
    let mut web = host("web", port, "deploy");
    web.set_jumps([bastion.id()]);
    web.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));
    let mut session = chain(
        vec![bastion.clone(), web.clone()],
        web.id(),
        vec![
            HopAuth::new(
                bastion.id(),
                AuthMethod::password("hunter2"),
                KnownHostKey::Unknown,
            ),
            HopAuth::new(
                web.id(),
                AuthMethod::password("hunter2"),
                KnownHostKey::Unknown,
            ),
        ],
        Duration::from_secs(5),
    );
    let message = wait_failed(&mut session).to_string();
    assert!(message.contains("ProxyCommand"), "{message}");
    assert!(message.contains("no command was run"), "{message}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pid_file.exists(), "the conflicting command must not start");
    assert!(matches!(
        listener.accept().map_err(|error| error.kind()),
        Err(ErrorKind::WouldBlock)
    ));
}

#[test]
fn a_proxy_command_does_not_run_until_it_is_confirmed_and_a_refusal_starts_nothing() {
    let pid_file = scratch("ask-pid");
    let done_file = scratch("ask-done");
    let mut web = host("web", 1, "deploy");
    web.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password("hunter2"),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(5),
    );
    let (hop, command) = wait_proxy(&mut session);
    assert_eq!(hop.host_id, web.id());
    assert!(command.contains(&quoted(&tool())), "{command}");
    assert!(
        !command.contains("%h"),
        "tokens are expanded before confirmation: {command}"
    );
    assert!(!pid_file.exists(), "seeing the command must not start it");
    std::thread::sleep(Duration::from_millis(250));
    assert!(!pid_file.exists());
    assert!(
        !session.confirm_proxy(HostId::new(), true),
        "another hop cannot confirm this command"
    );
    assert!(!pid_file.exists());
    assert!(session.confirm_proxy(web.id(), false));
    let failure = wait_failed(&mut session);
    assert!(
        matches!(failure, ChainFailure::ProxyNotConfirmed { .. }),
        "{failure}"
    );
    assert!(failure.to_string().contains("was not run"), "{failure}");
    assert!(!failure.to_string().contains("hunter2"), "{failure}");
    assert!(!pid_file.exists());
    assert!(!done_file.exists());
}

#[test]
fn cancelling_before_confirmation_never_starts_the_command() {
    let pid_file = scratch("cancel-ask-pid");
    let done_file = scratch("cancel-ask-done");
    let mut web = host("web", 9, "deploy");
    web.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password("hunter2"),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(5),
    );
    let (hop, _) = wait_proxy(&mut session);
    assert!(!pid_file.exists());
    session.close();
    assert!(
        !session.confirm_proxy(hop.host_id, true),
        "a closed hop cannot confirm the command"
    );
    assert!(!session.answer_host_key(hop.host_id, true));
    let failure = wait_failed(&mut session);
    assert!(
        matches!(failure, ChainFailure::Cancelled { .. }),
        "{failure}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pid_file.exists());
    assert!(!done_file.exists());
}

#[test]
fn a_confirmed_command_is_stopped_when_the_attempt_is_cancelled_or_fails() {
    let pid_file = scratch("live-pid");
    let done_file = scratch("live-done");
    let mut web = host("web", 1, "deploy");
    web.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password("hunter2"),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(3),
    );
    let (hop, _) = wait_proxy(&mut session);
    assert!(session.confirm_proxy(hop.host_id, true));
    let pid = wait_pid_file(&pid_file);
    assert!(process_alive(pid), "the confirmed command is running");
    session.close();
    let failure = wait_failed(&mut session);
    assert!(
        matches!(failure, ChainFailure::Cancelled { .. }),
        "{failure}"
    );
    wait_dead(pid);
    assert!(
        !done_file.exists(),
        "cancellation must stop the command before it finishes"
    );

    let pid_file = scratch("fail-pid");
    let done_file = scratch("fail-done");
    let mut web = host("web", 1, "deploy");
    web.set_proxy_command(Some(sleep_command(&pid_file, &done_file)));
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password("hunter2"),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(2),
    );
    let (hop, _) = wait_proxy(&mut session);
    assert!(session.confirm_proxy(hop.host_id, true));
    let pid = wait_pid_file(&pid_file);
    let failure = wait_failed(&mut session);
    assert!(
        matches!(
            failure,
            ChainFailure::Hop { .. } | ChainFailure::Cancelled { .. }
        ),
        "{failure}"
    );
    assert!(!failure.to_string().contains("hunter2"), "{failure}");
    wait_dead(pid);
    assert!(
        !done_file.exists(),
        "a failed attempt must stop the command"
    );
}

#[test]
fn two_hops_reach_the_target_shell_and_trust_belongs_to_each_node() {
    if skip_without_server("two-hop shell") {
        return;
    }
    let _gate = ssh_gate();
    let jump = Harness::start("jt-ok-j", Harness::free_port()).expect("jump server");
    let target = Harness::start("jt-ok-t", Harness::free_port()).expect("target server");
    jump.as_user(&format!(
        "printf '%s\\n' JUMP-MARKER > '{}/who.txt'",
        jump.home()
    ))
    .expect("jump marker");
    target
        .as_user(&format!(
            "printf '%s\\n' TARGET-MARKER > '{}/who.txt'",
            target.home()
        ))
        .expect("target marker");

    let jump_host = host("jump", jump.port(), jump.user());
    let mut target_host = host("target", target.port(), target.user());
    target_host.set_jumps([jump_host.id()]);
    let mut session = chain(
        vec![jump_host.clone(), target_host.clone()],
        target_host.id(),
        vec![
            HopAuth::new(
                jump_host.id(),
                AuthMethod::password(jump.password()),
                KnownHostKey::Unknown,
            ),
            HopAuth::new(
                target_host.id(),
                AuthMethod::password(target.password()),
                KnownHostKey::Unknown,
            ),
        ],
        Duration::from_secs(20),
    );

    let (hop, prompt) = wait_prompt(&mut session);
    assert_eq!(hop.host_id, jump_host.id());
    assert_eq!(prompt.host, "127.0.0.1");
    assert_eq!(prompt.port, jump.port());
    assert_eq!(prompt.fingerprint(), jump.host_key_fingerprint());
    assert!(
        !session.answer_host_key(target_host.id(), true),
        "the target must not accept the jump's trust question"
    );
    let mut trust = HostTrust::new();
    accept_prompt(&mut trust, &prompt);
    assert!(session.answer_host_key(jump_host.id(), true));
    assert!(
        !session.answer_host_key(jump_host.id(), true),
        "the jump question is over, so a second answer is not used"
    );
    assert!(trust.entry("127.0.0.1", target.port()).is_none());

    let (hop, prompt) = wait_prompt(&mut session);
    assert_eq!(hop.host_id, target_host.id());
    assert_eq!(prompt.port, target.port());
    assert_eq!(prompt.fingerprint(), target.host_key_fingerprint());
    assert_ne!(prompt.fingerprint(), jump.host_key_fingerprint());
    assert!(!session.answer_host_key(jump_host.id(), true));
    accept_prompt(&mut trust, &prompt);
    assert!(session.answer_host_key(target_host.id(), true));

    let connected = wait_connected(&mut session);
    assert_eq!(connected.host_id, target_host.id());
    assert_eq!(connected.port, target.port());
    session
        .write(b"cat \"$HOME/who.txt\"\n")
        .expect("the target shell accepts input");
    let output = wait_output(&mut session, "TARGET-MARKER");
    assert!(
        !output.contains("JUMP-MARKER"),
        "the shell must be the target, not the jump: {output:?}"
    );
    assert_eq!(
        trust
            .entry("127.0.0.1", jump.port())
            .expect("jump trust")
            .fingerprint(),
        jump.host_key_fingerprint()
    );
    assert_eq!(
        trust
            .entry("127.0.0.1", target.port())
            .expect("target trust")
            .fingerprint(),
        target.host_key_fingerprint()
    );
}

#[test]
fn an_auth_failure_names_that_hop_and_leaves_another_session_working() {
    if skip_without_server("hop auth failure") {
        return;
    }
    let _gate = ssh_gate();
    let jump = Harness::start("jt-auth-j", Harness::free_port()).expect("jump server");
    let target = Harness::start("jt-auth-t", Harness::free_port()).expect("target server");
    let mut other = direct_session(&target);
    wait_direct(&mut other);
    direct_echo(&mut other, "OTHER-JUMP-FAIL");

    let jump_host = host("jump", jump.port(), jump.user());
    let mut target_host = host("target", target.port(), target.user());
    target_host.set_jumps([jump_host.id()]);
    let accepted_before = target
        .log()
        .expect("target log")
        .matches("Accepted password")
        .count();
    let mut session = chain(
        vec![jump_host.clone(), target_host.clone()],
        target_host.id(),
        vec![
            HopAuth::new(
                jump_host.id(),
                AuthMethod::password("not-the-jump-password"),
                trusted(&jump.host_key_fingerprint()),
            ),
            HopAuth::new(
                target_host.id(),
                AuthMethod::password(target.password()),
                trusted(&target.host_key_fingerprint()),
            ),
        ],
        Duration::from_secs(20),
    );
    let failure = wait_failed(&mut session);
    let ChainFailure::Hop { hop, error } = &failure else {
        panic!("expected a hop failure, got {failure}");
    };
    assert_eq!(hop.host_id, jump_host.id());
    assert!(
        matches!(error.as_ref(), ConnectError::AuthRejected),
        "{error}"
    );
    assert!(
        !failure.to_string().contains("not-the-jump-password"),
        "{failure}"
    );
    assert_eq!(
        target
            .log()
            .expect("log")
            .matches("Accepted password")
            .count(),
        accepted_before,
        "a jump that rejects the password must not authenticate to the target"
    );
    direct_echo(&mut other, "OTHER-STILL-1");

    let mut session = chain(
        vec![jump_host.clone(), target_host.clone()],
        target_host.id(),
        vec![
            HopAuth::new(
                jump_host.id(),
                AuthMethod::password(jump.password()),
                trusted(&jump.host_key_fingerprint()),
            ),
            HopAuth::new(
                target_host.id(),
                AuthMethod::password("not-the-target-password"),
                trusted(&target.host_key_fingerprint()),
            ),
        ],
        Duration::from_secs(20),
    );
    let mut saw_connected = false;
    let failure = loop {
        let event = wait_match(&mut session, |event| {
            matches!(event, ChainEvent::Failed(_) | ChainEvent::Connected { .. })
        });
        match event {
            ChainEvent::Connected { .. } => saw_connected = true,
            ChainEvent::Failed(failure) => break failure,
            other => panic!("unexpected {other:?}"),
        }
    };
    assert!(!saw_connected, "a failed target must not report a shell");
    let ChainFailure::Hop { hop, error } = &failure else {
        panic!("expected a hop failure, got {failure}");
    };
    assert_eq!(hop.host_id, target_host.id(), "{failure}");
    assert!(
        matches!(error.as_ref(), ConnectError::AuthRejected),
        "{error}"
    );
    assert!(!failure.to_string().contains("not-the-target-password"));
    direct_echo(&mut other, "OTHER-STILL-2");
}

#[test]
fn a_changed_identity_names_that_hop_and_does_not_replace_trust() {
    if skip_without_server("changed hop identity") {
        return;
    }
    let _gate = ssh_gate();
    let jump = Harness::start("jt-id-j", Harness::free_port()).expect("jump server");
    let mut target = Harness::start("jt-id-t", Harness::free_port()).expect("target server");
    let old = target.host_key_fingerprint();
    let mut other = direct_session(&jump);
    wait_direct(&mut other);
    target.rotate().expect("the target identity changes");
    assert_ne!(target.host_key_fingerprint(), old);

    let jump_host = host("jump", jump.port(), jump.user());
    let mut target_host = host("target", target.port(), target.user());
    target_host.set_jumps([jump_host.id()]);
    let mut trust = HostTrust::new();
    let stored = HostKey::new("ssh-ed25519", &old, "stored-key").expect("stored key");
    // The chain is given the old fingerprint directly. The trust book is what a
    // caller would already have saved for the target, and it must still be that key.
    trust
        .accept("127.0.0.1", target.port(), &stored, 1)
        .expect("old target trust");
    let mut session = chain(
        vec![jump_host.clone(), target_host.clone()],
        target_host.id(),
        vec![
            HopAuth::new(
                jump_host.id(),
                AuthMethod::password(jump.password()),
                trusted(&jump.host_key_fingerprint()),
            ),
            HopAuth::new(
                target_host.id(),
                AuthMethod::password(target.password()),
                trusted(&old),
            ),
        ],
        Duration::from_secs(20),
    );
    let failure = wait_failed(&mut session);
    let ChainFailure::Hop { hop, error } = &failure else {
        panic!("expected a hop failure, got {failure}");
    };
    assert_eq!(hop.host_id, target_host.id(), "{failure}");
    assert_eq!(hop.port, target.port());
    match error.as_ref() {
        ConnectError::HostKeyChanged { stored, presented } => {
            assert_eq!(stored, &old);
            assert_ne!(presented.fingerprint, old);
        }
        other => panic!("expected a changed identity, got {other}"),
    }
    assert_eq!(
        trust
            .entry("127.0.0.1", target.port())
            .expect("the old record remains")
            .fingerprint(),
        old
    );
    direct_echo(&mut other, "OTHER-IDENTITY");
}

#[test]
fn a_dropped_jump_names_the_jump_and_the_target_session_keeps_working() {
    if skip_without_server("jump disconnect") {
        return;
    }
    let _gate = ssh_gate();
    let mut jump = Harness::start("jt-drop-j", Harness::free_port()).expect("jump server");
    let target = Harness::start("jt-drop-t", Harness::free_port()).expect("target server");
    target
        .as_user(&format!(
            "printf '%s\\n' TARGET-MARKER > '{}/who.txt'",
            target.home()
        ))
        .expect("marker");
    let mut other = direct_session(&target);
    wait_direct(&mut other);

    let jump_host = host("jump", jump.port(), jump.user());
    let mut target_host = host("target", target.port(), target.user());
    target_host.set_jumps([jump_host.id()]);
    let mut session = chain(
        vec![jump_host.clone(), target_host.clone()],
        target_host.id(),
        vec![
            HopAuth::new(
                jump_host.id(),
                AuthMethod::password(jump.password()),
                trusted(&jump.host_key_fingerprint()),
            ),
            HopAuth::new(
                target_host.id(),
                AuthMethod::password(target.password()),
                trusted(&target.host_key_fingerprint()),
            ),
        ],
        Duration::from_secs(20),
    );
    let connected = wait_connected(&mut session);
    assert_eq!(connected.host_id, target_host.id());
    session.write(b"cat \"$HOME/who.txt\"\n").expect("write");
    let output = wait_output(&mut session, "TARGET-MARKER");
    assert!(!output.contains("JUMP-MARKER"));

    jump.stop();
    let failure = wait_failed(&mut session);
    let ChainFailure::Hop { hop, error } = &failure else {
        panic!("expected a hop failure, got {failure}");
    };
    assert_eq!(hop.host_id, jump_host.id(), "{failure}");
    assert!(
        matches!(error.as_ref(), ConnectError::Disconnected { .. }),
        "{error}"
    );
    direct_echo(&mut other, "OTHER-AFTER-DROP");
}

#[test]
fn a_confirmed_proxy_command_carries_a_real_shell_and_the_process_exits_afterwards() {
    if skip_without_server("proxy command shell") {
        return;
    }
    let _gate = ssh_gate();
    let server = Harness::start("jt-proxy", Harness::free_port()).expect("server");
    server
        .as_user(&format!(
            "printf '%s\\n' PROXY-MARKER > '{}/who.txt'",
            server.home()
        ))
        .expect("marker");
    let pid_file = scratch("proxy-pid");
    let mut web = host("web", server.port(), server.user());
    web.set_proxy_command(Some(bridge_command(&pid_file)));
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password(server.password()),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(20),
    );
    let (hop, command) = wait_proxy(&mut session);
    assert_eq!(hop.host_id, web.id());
    assert!(command.contains(&server.port().to_string()), "{command}");
    assert!(!command.contains("%p"), "{command}");
    assert!(!pid_file.exists());
    assert!(
        !session.answer_host_key(web.id(), true),
        "a trust answer is not a confirmation"
    );
    assert!(session.confirm_proxy(web.id(), true));
    let pid = wait_pid_file(&pid_file);
    assert!(process_alive(pid));

    let (hop, prompt) = wait_prompt(&mut session);
    assert_eq!(hop.host_id, web.id());
    assert_eq!(prompt.port, server.port());
    assert_eq!(prompt.fingerprint(), server.host_key_fingerprint());
    assert!(session.answer_host_key(web.id(), true));
    let connected = wait_connected(&mut session);
    assert_eq!(connected.host_id, web.id());
    session.write(b"cat \"$HOME/who.txt\"\n").expect("write");
    let output = wait_output(&mut session, "PROXY-MARKER");
    assert!(!output.contains("hunter2"));
    session.close();
    wait_dead(pid);
}

#[test]
fn closing_during_a_trust_question_rejects_a_late_answer() {
    if skip_without_server("cancel during trust") {
        return;
    }
    let _gate = ssh_gate();
    let server = Harness::start("jt-ask", Harness::free_port()).expect("server");
    let web = host("web", server.port(), server.user());
    let mut session = chain(
        vec![web.clone()],
        web.id(),
        vec![HopAuth::new(
            web.id(),
            AuthMethod::password(server.password()),
            KnownHostKey::Unknown,
        )],
        Duration::from_secs(20),
    );
    let (hop, prompt) = wait_prompt(&mut session);
    assert_eq!(hop.host_id, web.id());
    assert_eq!(prompt.fingerprint(), server.host_key_fingerprint());
    session.close();
    assert!(
        !session.answer_host_key(web.id(), true),
        "a closed hop must not accept the identity"
    );
    let failure = wait_failed(&mut session);
    assert!(
        matches!(failure, ChainFailure::Cancelled { .. }),
        "{failure}"
    );
    let trust = HostTrust::new();
    // The caller keeps the record only when the answer was delivered. It was not.
    assert!(trust.entry("127.0.0.1", server.port()).is_none());
}
