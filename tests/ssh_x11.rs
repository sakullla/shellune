//! X11 forwarding against a real OpenSSH server and a real X server (R11).
//!
//! Every test here talks to /usr/sbin/sshd and to Xvfb, not to a stand-in: a real
//! handshake, a real `x11-req`, a real remote `xauth` entry, real remote programs that
//! connect to a display whose cookie is enforced. The X server is started with an
//! authority file, so a program that reaches it has really presented the right cookie;
//! a bridge that dropped it or forwarded the session cookie unchanged would be refused
//! instead of quietly passing.
//!
//! The disposable server, the display server and the two environment variables the
//! product reads (`DISPLAY`, `XAUTHORITY`) belong to the whole process, so the tests of
//! this file run one at a time. When this host cannot start an OpenSSH server or has no
//! Xvfb, a test prints why it cannot run and returns: a skipped end-to-end check is a
//! gap in evidence, never a pass.

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use shellune_core::{AuthMethod, HostConfig, X11Forwarding};
use shellune_ssh::testserver::{DisplayServer, Harness, X11Server};
use shellune_ssh::{
    ChainEvent, ChainRequest, ChainSession, HopAuth, KnownHostKey, PtySize, X11Status,
};

/// How long any single wait may take.
const PATIENCE: Duration = Duration::from_secs(45);

/// The tests in this file set process-wide environment variables, so they run alone.
fn gate() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// Whether this host can run the end-to-end check at all.
fn skip(what: &str, needs_display: bool) -> bool {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so X11 \
             forwarding was not verified end to end and is not claimed here"
        );
        return true;
    }
    if needs_display && !DisplayServer::is_available() {
        eprintln!(
            "SKIPPED {what}: this host has no Xvfb to forward to, so no window was opened and \
             nothing is claimed; install Xvfb on the machine that runs the server to run this test"
        );
        return true;
    }
    false
}

/// The two variables the product reads, restored when the test ends.
struct DisplayEnv {
    display: Option<OsString>,
    authority: Option<OsString>,
}

impl DisplayEnv {
    fn set(display: Option<&str>, authority: Option<&Path>) -> Self {
        let previous = Self {
            display: std::env::var_os("DISPLAY"),
            authority: std::env::var_os("XAUTHORITY"),
        };
        // The product reads these from its own environment, which is this process's:
        // that is how a real user's shell hands it a display, and the tests drive the
        // same path rather than a test-only parameter.
        unsafe {
            match display {
                Some(display) => std::env::set_var("DISPLAY", display),
                None => std::env::remove_var("DISPLAY"),
            }
            match authority {
                Some(authority) => std::env::set_var("XAUTHORITY", authority),
                None => std::env::remove_var("XAUTHORITY"),
            }
        }
        previous
    }
}

impl Drop for DisplayEnv {
    fn drop(&mut self) {
        unsafe {
            match self.display.take() {
                Some(value) => std::env::set_var("DISPLAY", value),
                None => std::env::remove_var("DISPLAY"),
            }
            match self.authority.take() {
                Some(value) => std::env::set_var("XAUTHORITY", value),
                None => std::env::remove_var("XAUTHORITY"),
            }
        }
    }
}

/// Starts a connection to the disposable server with one X11 mode.
fn connect(server: &Harness, mode: X11Forwarding) -> ChainSession {
    let host = HostConfig::new("x11-test", "127.0.0.1", server.port(), server.user())
        .expect("a valid test host");
    let known = KnownHostKey::Trusted {
        fingerprint: server.host_key_fingerprint(),
    };
    let auth = HopAuth::new(
        host.id(),
        AuthMethod::key_file(server.client_key().to_string_lossy().into_owned(), None),
        known,
    );
    ChainSession::connect(
        ChainRequest::new(
            vec![host.clone()],
            host.id(),
            vec![auth],
            PtySize::new(100, 30),
        )
        .with_x11(mode),
    )
}

/// Writes one line into the remote shell.
fn write(session: &mut ChainSession, line: &str) {
    session
        .write(format!("{line}\n").as_bytes())
        .expect("the remote shell takes input");
}

/// The display statuses and output a session reported until it was connected.
struct Started {
    x11: Vec<X11Status>,
    output: String,
}

/// Waits until the shell is running, keeping every X11 status and byte on the way.
fn wait_connected(session: &mut ChainSession) -> Started {
    let deadline = Instant::now() + PATIENCE;
    let mut started = Started {
        x11: Vec::new(),
        output: String::new(),
    };
    loop {
        // A batch is applied whole: the notice and the display statuses can arrive in
        // the same drain, and returning from the middle of it would drop the rest.
        let mut connected = false;
        for event in session.drain() {
            match event {
                ChainEvent::X11 { status, .. } => started.x11.push(status),
                ChainEvent::Connected { .. } => connected = true,
                ChainEvent::Data(bytes) => {
                    started.output.push_str(&String::from_utf8_lossy(&bytes))
                }
                ChainEvent::Failed(failure) => panic!("the attempt failed: {failure}"),
                _ => {}
            }
        }
        if connected {
            return started;
        }
        if Instant::now() > deadline {
            panic!("the attempt neither connected nor failed in time");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits for the display request to be decided, dropping unrelated output.
fn wait_x11(session: &mut ChainSession, already: Vec<X11Status>) -> X11Status {
    // The request may already have been decided before the shell was announced, which
    // is the common case for a display that could not be reached at all.
    let mut statuses = already;
    if let Some(decided) = statuses
        .iter()
        .rev()
        .find(|status| !matches!(status, X11Status::Enabling))
    {
        return decided.clone();
    }
    let deadline = Instant::now() + PATIENCE;
    let mut output = String::new();
    loop {
        for event in session.drain() {
            match event {
                ChainEvent::X11 { status, .. } => {
                    statuses.push(status);
                    if let Some(decided) = statuses
                        .iter()
                        .rev()
                        .find(|status| !matches!(status, X11Status::Enabling))
                    {
                        return decided.clone();
                    }
                }
                ChainEvent::Data(bytes) => output.push_str(&String::from_utf8_lossy(&bytes)),
                ChainEvent::Failed(failure) => {
                    panic!("the session failed while waiting for X11: {failure}")
                }
                _ => {}
            }
        }
        if Instant::now() > deadline {
            panic!(
                "the display request was never decided; statuses {statuses:?}, output {output:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the remote shell printed a phrase, returning everything received.
fn wait_for_output(session: &mut ChainSession, needle: &str, already: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    let mut output = already.to_owned();
    loop {
        for event in session.drain() {
            match event {
                ChainEvent::Data(bytes) => output.push_str(&String::from_utf8_lossy(&bytes)),
                ChainEvent::Failed(failure) => panic!("the session failed: {failure}"),
                ChainEvent::Exited { code } => panic!(
                    "the remote shell exited ({code:?}) before printing {needle:?}; received {output:?}"
                ),
                _ => {}
            }
        }
        if output.contains(needle) {
            return output;
        }
        if Instant::now() > deadline {
            panic!("the remote shell never printed {needle:?}; received {output:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How many times the server accepted a login, which is what "no second
/// authentication" is about.
fn accepted_logins(server: &Harness) -> usize {
    server
        .log()
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("Accepted publickey"))
        .count()
}

#[test]
fn a_remote_program_reaches_this_machines_display_and_a_second_one_needs_no_login() {
    let _serial = gate();
    if skip("X11 forwarding to a local display", true) {
        return;
    }
    let server = Harness::start_with_x11("x11-ready", Harness::free_port(), X11Server::Enabled)
        .expect("the disposable server starts");
    let display = DisplayServer::start("x11-ready").expect("the display server starts");
    let _env = DisplayEnv::set(Some(&display.display()), Some(&display.authority()));
    // The log holds every run of this harness; the count that matters is the one this
    // session adds.
    let logins_before = accepted_logins(&server);

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    let status = wait_x11(&mut session, started.x11);
    assert!(
        matches!(status, X11Status::Ready { .. }),
        "a server that allows X11 and a display that answers must end as ready, got {status:?}"
    );

    // The first remote program: its own connection to the display, through the
    // session's X11 channel and this machine's real cookie.
    write(&mut session, "xdpyinfo 2>&1 | head -2");
    let output = wait_for_output(&mut session, "version number", &started.output);
    assert!(
        output.contains("name of display") || output.contains("version number"),
        "the remote program read this machine's display: {output:?}"
    );

    // The second program, started from the same session: another X11 channel, and no
    // second authentication - the server's own log is the evidence for that.
    write(
        &mut session,
        "xdpyinfo >/dev/null 2>&1 && echo SECOND-$((2 + 2))",
    );
    let output = wait_for_output(&mut session, "SECOND-4", &output);
    assert_eq!(
        accepted_logins(&server),
        logins_before + 1,
        "two programs in one session must share one authenticated connection: {}",
        server.log().unwrap_or_default()
    );
    assert!(
        output.contains("SECOND-4"),
        "the second program reached the display: {output:?}"
    );

    // The cookie the server was given is this session's own: the real one of this
    // machine is never sent, so a leaked server cannot authenticate to the display.
    write(&mut session, "xauth list 2>/dev/null");
    let listed = wait_for_output(&mut session, "MIT-MAGIC-COOKIE-1", &output);
    assert!(
        !listed.contains(&display.cookie_hex()),
        "the remote authority file must not hold this machine's cookie"
    );

    // A window: the program keeps its connection open, and the X side lists it. This
    // is the "shows up on this computer" half, not only "the connection was accepted".
    write(&mut session, "xlogo >/dev/null 2>&1 &");
    let deadline = Instant::now() + PATIENCE;
    let mut windows = 0;
    while Instant::now() < deadline {
        windows = display.window_count().unwrap_or(0);
        if windows > 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        windows > 1,
        "the remote program's window is in this machine's display, windows: {windows}"
    );

    // Closing the session ends its bridge: the display is left with no connection from
    // the forwarded programs.
    session.close();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut open = usize::MAX;
    while Instant::now() < deadline {
        open = display.connections().unwrap_or(0);
        if open == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        open, 0,
        "a closed session left no display connection behind"
    );
}

#[test]
fn a_session_without_x11_never_asks_for_a_display() {
    let _serial = gate();
    if skip("a session without X11 forwarding", false) {
        return;
    }
    let server = Harness::start_with_x11("x11-off", Harness::free_port(), X11Server::Enabled)
        .expect("the disposable server starts");

    let mut session = connect(&server, X11Forwarding::Off);
    let started = wait_connected(&mut session);
    assert_eq!(
        started.x11,
        vec![X11Status::Off],
        "an off session says so and asks for nothing: {:?}",
        started.x11
    );
    // The server sets DISPLAY only when a session asked it to; an empty value proves
    // nothing was requested and nothing was set behind the user's back.
    write(
        &mut session,
        "printf 'DISPLAY-%s-END\\n' \"${DISPLAY:-none}\"",
    );
    let output = wait_for_output(&mut session, "DISPLAY-none-END", &started.output);
    assert!(
        output.contains("DISPLAY-none-END"),
        "a session that did not ask for X11 has no remote DISPLAY: {output:?}"
    );
}

#[test]
fn a_machine_without_a_display_says_so_and_keeps_the_shell() {
    let _serial = gate();
    if skip("X11 forwarding without a local display", false) {
        return;
    }
    let server = Harness::start_with_x11("x11-nodisplay", Harness::free_port(), X11Server::Enabled)
        .expect("the disposable server starts");
    let _env = DisplayEnv::set(None, None);

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("no local X display"),
                "the reason names the missing display: {reason}"
            );
            assert!(
                next_step.to_lowercase().contains("x server"),
                "the next step says what to start: {next_step}"
            );
        }
        other => panic!("a machine without a display must report a failure, got {other:?}"),
    }

    // The session itself is a normal SSH session: it connects, runs commands and does
    // not claim a display it never had.
    write(&mut session, "echo ALIVE-$((40 + 2))");
    let output = wait_for_output(&mut session, "ALIVE-42", &started.output);
    assert!(output.contains("ALIVE-42"), "{output:?}");
}

#[test]
fn a_server_that_forbids_x11_is_reported_and_the_shell_still_works() {
    let _serial = gate();
    if skip("a server that forbids X11 forwarding", true) {
        return;
    }
    let server = Harness::start_with_x11("x11-disabled", Harness::free_port(), X11Server::Disabled)
        .expect("the disposable server starts");
    let display = DisplayServer::start("x11-disabled").expect("the display server starts");
    let _env = DisplayEnv::set(Some(&display.display()), Some(&display.authority()));

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("refused"),
                "a refusal is described as one: {reason}"
            );
            assert!(
                next_step.contains("X11Forwarding") && next_step.contains("xauth"),
                "the next step names both server-side causes: {next_step}"
            );
        }
        other => panic!("a server that forbids X11 must report a failure, got {other:?}"),
    }
    assert!(
        server
            .log()
            .unwrap_or_default()
            .contains("X11 forwarding disabled"),
        "the server really refused for this reason: {}",
        server.log().unwrap_or_default()
    );

    write(&mut session, "echo SHELL-$((6 * 7))");
    let output = wait_for_output(&mut session, "SHELL-42", &started.output);
    assert!(output.contains("SHELL-42"), "{output:?}");
}

#[test]
fn a_server_without_xauth_reports_that_cause_and_the_shell_still_works() {
    let _serial = gate();
    if skip("a server without xauth", true) {
        return;
    }
    let server =
        Harness::start_with_x11("x11-noxauth", Harness::free_port(), X11Server::WithoutXauth)
            .expect("the disposable server starts");
    let display = DisplayServer::start("x11-noxauth").expect("the display server starts");
    let _env = DisplayEnv::set(Some(&display.display()), Some(&display.authority()));

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("refused"),
                "the refusal is described as one: {reason}"
            );
            assert!(
                next_step.to_lowercase().contains("xauth"),
                "a server without xauth is answered with the fix that installs it: {next_step}"
            );
        }
        other => panic!("a server without xauth cannot forward, got {other:?}"),
    }
    assert!(
        server
            .log()
            .unwrap_or_default()
            .to_lowercase()
            .contains("xauth"),
        "the server really refused because it has no xauth: {}",
        server.log().unwrap_or_default()
    );

    write(&mut session, "echo XAUTH-$((6 * 7))");
    let output = wait_for_output(&mut session, "XAUTH-42", &started.output);
    assert!(output.contains("XAUTH-42"), "{output:?}");
}
