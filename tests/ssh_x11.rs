//! X11 forwarding against a real OpenSSH server and a real X server (R11).
//!
//! Every test here talks to /usr/sbin/sshd and to Xvfb, not to a stand-in: a real
//! handshake, a real `x11-req`, a real remote `xauth` entry, real remote programs that
//! connect to a display whose cookie is enforced. The X server is started with an
//! authority file, so a program that reaches it has really presented the right cookie;
//! a bridge that dropped it or forwarded the session cookie unchanged would be refused
//! instead of quietly passing.
//!
//! The restricted mode is exercised with the same `xauth` command OpenSSH's client uses
//! (`xauth generate <display> MIT-MAGIC-COOKIE-1 untrusted`), through a stand-in on
//! `PATH` that runs the real `xauth` of the Linux environment the display belongs to.
//! The entry it issues goes into a private file of the session, and what that test checks
//! is the issuance, the key the entry is written under, and that the authority file of
//! this machine is left byte for byte as it was. The trusted mode is exercised without
//! any `xauth` at all, which is the difference between the two credentials.
//!
//! A display server that answers `xauth generate` and then refuses the authorization it
//! issued cannot complete the restricted path: no remote program authenticates through
//! it. Debian's Xvfb 21.1.16 is such a server (only what is in its own `-auth` file is
//! accepted). The display is asked about that itself before the product is involved, and
//! the two answers have to agree: where the display honours what it issues the restricted
//! session must end as ready, a remote program must reach that display, and a second
//! program in the same session must not authenticate again; where it does not, the
//! product must report that refusal and must not claim a display it cannot forward to.
//! The refusal is a printed gap, not a pass. `SHELLUNE_X11_DISPLAY` names a display the
//! machine is already running (with `SHELLUNE_X11_AUTHORITY` when that file is not the
//! usual one) so the honoured path can be run against a server that enforces SECURITY,
//! instead of against the harness's Xvfb.
//!
//! The disposable server, the display server and the environment variables the product
//! reads (`DISPLAY`, `XAUTHORITY`, `PATH`) belong to the whole process, so the tests of
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

/// The variables the product reads, restored when the test ends.
struct DisplayEnv {
    display: Option<OsString>,
    authority: Option<OsString>,
    path: Option<OsString>,
}

impl DisplayEnv {
    /// Shows the product a display, and puts `xauth` where it looks for it.
    ///
    /// `xauth` is the directory to put in front of `PATH`; the rest of `PATH` stays, so
    /// the harness itself can still run `wsl.exe`.
    fn set(display: Option<&str>, authority: Option<&Path>, xauth: Option<&Path>) -> Self {
        let previous = Self {
            display: std::env::var_os("DISPLAY"),
            authority: std::env::var_os("XAUTHORITY"),
            path: std::env::var_os("PATH"),
        };
        let path = xauth.map(|xauth| {
            let existing = std::env::var_os("PATH").unwrap_or_default();
            std::env::join_paths(
                std::iter::once(xauth.to_path_buf()).chain(std::env::split_paths(&existing)),
            )
            .expect("a search path")
        });
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
            if let Some(path) = &path {
                std::env::set_var("PATH", path);
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
            match self.path.take() {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
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

/// Waits until the display has no client left, or reports how many are still there.
fn wait_for_no_connections(display: &DisplayServer) -> usize {
    wait_for_connection_count(display, 0)
}

/// Waits until the display reports `target` clients, or reports how many are still there.
fn wait_for_connection_count(display: &DisplayServer, target: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut open = usize::MAX;
    while Instant::now() < deadline {
        open = display.connections().unwrap_or(usize::MAX);
        if open == target {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    open
}

#[test]
fn remote_programs_reach_this_machines_display_over_one_authenticated_connection() {
    let _serial = gate();
    if skip("X11 forwarding to a local display", true) {
        return;
    }
    let server = Harness::start_with_x11("x11-ready", Harness::free_port(), X11Server::Enabled)
        .expect("the disposable server starts");
    let display = DisplayServer::start("x11-ready").expect("the display server starts");
    let xauth = display.xauth_path();
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&xauth),
    );
    // The log holds every run of this harness; the count that matters is the one this
    // session adds.
    let logins_before = accepted_logins(&server);
    // The entry this machine's own programs authenticate with, and the file it lives in.
    let own = display.authority_record();
    let authority_before = std::fs::read(display.authority()).expect("the authority file");

    let mut session = connect(&server, X11Forwarding::Trusted);
    let started = wait_connected(&mut session);
    let status = wait_x11(&mut session, started.x11);
    assert!(
        matches!(status, X11Status::Ready { .. }),
        "a server that allows X11 and a display that answers must end as ready, got {status:?}"
    );

    // The first remote program: its own connection to the display, through the
    // session's X11 channel and the credential of this machine.
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

    // The cookie the server was given is this session's own: the local one is never
    // sent, so a leaked server cannot authenticate to the display.
    write(&mut session, "xauth list 2>/dev/null");
    let listed = wait_for_output(&mut session, "MIT-MAGIC-COOKIE-1", &output);
    assert!(
        !listed.contains(&own.cookie_hex),
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
    assert_eq!(
        wait_for_no_connections(&display),
        0,
        "a closed session left no display connection behind"
    );
    // A session that only reads the authority file leaves it exactly as it was.
    assert_eq!(
        std::fs::read(display.authority()).expect("the authority file"),
        authority_before,
        "a forwarded session must not write to this machine's authority file"
    );
    assert_eq!(display.authority_record(), own);
}

#[test]
fn a_restricted_session_asks_the_display_for_an_untrusted_cookie_and_keeps_the_authority_file() {
    let _serial = gate();
    if skip("restricted X11 forwarding to a local display", true) {
        return;
    }
    let server =
        Harness::start_with_x11("x11-restricted", Harness::free_port(), X11Server::Enabled)
            .expect("the disposable server starts");
    // `SHELLUNE_X11_DISPLAY` selects a server that already enforces SECURITY. The
    // harness's own Xvfb does not, and a pass on that server would be the refusal path.
    let display =
        DisplayServer::forwarding_display("x11-restricted").expect("the display server starts");
    let xauth = display.xauth_path();
    // Whether this display server honours what it issues is a property of the server, so
    // it is asked of the server before the product is involved.
    let honoured = display.honours_issued_cookies();
    eprintln!(
        "restricted display {} honoured={honoured}",
        display.display()
    );
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&xauth),
    );
    // The entry this machine's own programs authenticate with, read before the session
    // touches anything. Its key is the ordinary one of a local display, which is also
    // the key an untrusted entry is written under: a mode that wrote into this file
    // would replace it, and that is what this test is looking for.
    let own = display.authority_record();
    let authority_before = std::fs::read(display.authority()).expect("the authority file");
    eprintln!(
        "AUTHORITY path={} bytes={}",
        display.authority().display(),
        authority_before.len()
    );
    // Clients the display already had, and logins the server already accepted. A second
    // program has to leave both of those where they are: one more login, no more clients
    // once the session is gone.
    let clients_before = if display.keeps_other_clients() {
        display
            .connections()
            .expect("the display reports its clients")
    } else {
        0
    };
    let logins_before = accepted_logins(&server);
    eprintln!("DISPLAY_CLIENTS before={clients_before}");

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    let status = wait_x11(&mut session, started.x11);
    eprintln!("restricted status={status:?}");
    if honoured {
        // The display accepts what it issues, so the session forwards: ready, and the
        // remote program really reaches the display through the restricted credential.
        match &status {
            X11Status::Ready { display: ready } => assert_eq!(
                ready,
                &display.display(),
                "the session must bridge to this machine's display, got {status:?}"
            ),
            _ => panic!(
                "a server that allows X11 and a display that honours its own cookie must end as ready, got {status:?}"
            ),
        }

        // The credential the local display issued is never the one the server is told: the
        // remote programs get this session's own cookie and the bridge translates it.
        write(&mut session, "xauth list 2>/dev/null");
        let listed = wait_for_output(&mut session, "MIT-MAGIC-COOKIE-1", &started.output);
        assert!(
            !listed.contains(&own.cookie_hex),
            "the remote authority file must not hold this machine's cookie"
        );

        let version = display
            .server_version_line()
            .expect("the local display names its X.Org version");
        write(&mut session, "xdpyinfo 2>&1");
        let output = wait_for_output(&mut session, &version, &listed);
        assert!(
            output.contains(&version),
            "the remote program reached this machine's display ({version}): {output:?}"
        );
        assert!(
            output.contains("name of display"),
            "the remote program named the display it reached: {output:?}"
        );

        // The second program, in the same session: another X11 channel, and no second
        // authentication. The server's log is the evidence for that.
        write(
            &mut session,
            "xdpyinfo >/dev/null 2>&1 && echo SECOND-$((2 + 2))",
        );
        let output = wait_for_output(&mut session, "SECOND-4", &output);
        assert!(
            output.contains("SECOND-4"),
            "the second program reached the display: {output:?}"
        );
        assert_eq!(
            accepted_logins(&server),
            logins_before + 1,
            "two programs in one restricted session must share one authenticated connection: {}",
            server.log().unwrap_or_default()
        );
        eprintln!(
            "SECOND_PROGRAM reached the display on one login (logins now {})",
            logins_before + 1
        );

        let windows_before = display.window_count().unwrap_or(0);
        write(&mut session, "xlogo >/dev/null 2>&1 &");
        let deadline = Instant::now() + PATIENCE;
        let mut windows = windows_before;
        while Instant::now() < deadline {
            windows = display.window_count().unwrap_or(0);
            if windows > windows_before {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            windows > windows_before,
            "the remote program's window is in this machine's display: {windows_before} then {windows}"
        );
        eprintln!("REMOTE_WINDOW {windows_before} -> {windows}");
    } else {
        // The display answers `xauth generate` and then refuses the authorization: the
        // product has to say exactly that. Reporting a display that no remote program can
        // use would be a pass on paper only.
        match &status {
            X11Status::Failed { reason, next_step } => {
                assert!(
                    reason.contains("refuses") && reason.to_lowercase().contains("restricted"),
                    "the reason is the display's refusal of the cookie it issued: {reason}"
                );
                assert!(
                    next_step.to_lowercase().contains("trusted"),
                    "the next step offers the trusted mode as a choice: {next_step}"
                );
                assert!(
                    next_step.contains("Xorg")
                        || next_step.contains("XWayland")
                        || next_step.contains("XQuartz"),
                    "the next step names a display server that enforces the extension: {next_step}"
                );
            }
            other => panic!(
                "a display that refuses what it issues must be reported as a failure, not {other:?}"
            ),
        }
        eprintln!(
            "GAP restricted X11 forwarding: this display server registers the X11 SECURITY \
             extension, answers `xauth generate … untrusted`, and then refuses the \
             authorization it issued (measured here on Debian's Xvfb 21.1.16, and reproduced \
             by the owner: the cookie is accepted only once it is in the server's own -auth \
             file). A remote program therefore cannot reach this machine's display under the \
             restricted mode on this server. What is checked here is the issuance, the key it \
             is written under, the untouched authority file, and that the product reports the \
             refusal instead of a display it cannot forward to. Restricted forwarding that \
             really shows a remote program needs a display server that enforces the extension \
             (Xorg, XWayland or XQuartz) and is a delivery gap here, not a pass. The \
             disposition is recorded in docs/design/02-shell-and-design-system.md (X11 \
             forwarding): the restricted mode's display-side acceptance is not passed, and \
             the condition for it is that display server."
        );

        // Nothing was requested of the server for X11, so the remote shell has no DISPLAY
        // and the session is a plain SSH session that keeps working.
        write(
            &mut session,
            "printf 'DISPLAY-%s-END\\n' \"${DISPLAY:-none}\"",
        );
        let output = wait_for_output(&mut session, "DISPLAY-none-END", &started.output);
        assert!(
            output.contains("DISPLAY-none-END"),
            "a session that could not forward was given no remote DISPLAY: {output:?}"
        );
    }

    session.close();
    let clients_after = if display.keeps_other_clients() {
        clients_before
    } else {
        0
    };
    assert_eq!(
        wait_for_connection_count(&display, clients_after),
        clients_after,
        "a closed session left no display connection of its own behind"
    );

    // The restricted mode asked the display for an untrusted cookie, and that cookie is
    // the credential it forwards with: a different one from this machine's own, written
    // under the same key, which is why writing it to the authority file would have taken
    // this machine's own entry away.
    let issued = display
        .issued_record()
        .expect("the restricted mode asked the display for an untrusted cookie");
    assert_eq!(
        issued.key(),
        own.key(),
        "the issued entry is written under the key of the display's own entry, so it can only be kept apart in a file of its own"
    );
    assert_ne!(
        issued.cookie_hex, own.cookie_hex,
        "the restricted mode must forward with a cookie the display issued, not with this machine's own"
    );

    // And the file this machine's own programs use is byte for byte what it was: no entry
    // was added, replaced or rewritten, so nothing about their access changed.
    assert_eq!(
        std::fs::read(display.authority()).expect("the authority file"),
        authority_before,
        "a restricted session must leave the authority file of this machine untouched"
    );
    assert_eq!(
        display.authority_record(),
        own,
        "the display's own trusted entry is still there, unchanged"
    );

    // A trusted session on the same display afterwards presents the trusted cookie: it
    // needs no xauth at all, and the file it reads that cookie from is still the file the
    // restricted session did not touch.
    {
        let broken = display.broken_xauth_path();
        let _env = DisplayEnv::set(
            Some(&display.display()),
            Some(&display.authority()),
            Some(&broken),
        );
        let mut trusted = connect(&server, X11Forwarding::Trusted);
        let started = wait_connected(&mut trusted);
        let status = wait_x11(&mut trusted, started.x11);
        assert!(
            matches!(status, X11Status::Ready { .. }),
            "the trusted mode after a restricted session needs no issued cookie, got {status:?}"
        );
        write(&mut trusted, "xdpyinfo 2>&1 | head -2");
        let output = wait_for_output(&mut trusted, "version number", &started.output);
        assert!(
            output.contains("version number"),
            "the trusted session reached the display with the trusted cookie: {output:?}"
        );
        trusted.close();
        assert_eq!(
            wait_for_connection_count(&display, clients_before),
            clients_before,
            "the trusted session's bridge was dropped"
        );
        assert_eq!(
            std::fs::read(display.authority()).expect("the authority file"),
            authority_before,
            "a trusted session reads the authority file and writes nothing to it"
        );
        assert_eq!(
            display.authority_record(),
            own,
            "the trusted session used the display's own entry, not one issued earlier"
        );
    }
}

#[test]
fn a_trusted_session_forwards_without_needing_a_restricted_cookie() {
    let _serial = gate();
    if skip("a trusted X11 session", true) {
        return;
    }
    let server = Harness::start_with_x11("x11-trusted", Harness::free_port(), X11Server::Enabled)
        .expect("the disposable server starts");
    let display = DisplayServer::start("x11-trusted").expect("the display server starts");
    // No working xauth on this machine at all: the trusted mode forwards with the
    // display's own cookie and never asks for another one.
    let broken = display.broken_xauth_path();
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&broken),
    );

    let mut session = connect(&server, X11Forwarding::Trusted);
    let started = wait_connected(&mut session);
    let status = wait_x11(&mut session, started.x11);
    assert!(
        matches!(status, X11Status::Ready { .. }),
        "the trusted mode needs no restricted cookie, got {status:?}"
    );
    write(&mut session, "xdpyinfo 2>&1 | head -2");
    let output = wait_for_output(&mut session, "version number", &started.output);
    assert!(
        output.contains("version number"),
        "the remote program reached the display: {output:?}"
    );
    session.close();
    assert_eq!(
        wait_for_no_connections(&display),
        0,
        "the bridge was dropped"
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
    let _env = DisplayEnv::set(None, None, None);

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
fn a_display_without_the_security_extension_stops_the_restricted_mode() {
    let _serial = gate();
    if skip("restricted forwarding to a display without SECURITY", true) {
        return;
    }
    let server =
        Harness::start_with_x11("x11-nosecurity", Harness::free_port(), X11Server::Enabled)
            .expect("the disposable server starts");
    let display =
        DisplayServer::start_without_security("x11-nosecurity").expect("the display server starts");
    let xauth = display.xauth_path();
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&xauth),
    );

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("no restricted X11 cookie"),
                "the reason is the missing untrusted cookie: {reason}"
            );
            assert!(
                reason.to_lowercase().contains("security"),
                "the reason is what the display said: {reason}"
            );
            // The way out is a choice the user makes, never a silent fallback.
            assert!(
                next_step.to_lowercase().contains("trusted"),
                "the next step names the trusted mode as a choice: {next_step}"
            );
        }
        other => panic!("a display without SECURITY cannot restrict, got {other:?}"),
    }
    // Nothing was forwarded: no display connection was made on this session's behalf.
    assert_eq!(
        display.connections().unwrap_or(0),
        0,
        "a refused restricted session must not reach the display"
    );
    write(&mut session, "echo PLAIN-$((6 * 7))");
    let output = wait_for_output(&mut session, "PLAIN-42", &started.output);
    assert!(output.contains("PLAIN-42"), "{output:?}");
}

#[test]
fn a_restricted_session_whose_xauth_fails_reports_it_instead_of_forwarding_trusted() {
    let _serial = gate();
    if skip("restricted forwarding with a failing xauth", true) {
        return;
    }
    let server = Harness::start_with_x11("x11-badxauth", Harness::free_port(), X11Server::Enabled)
        .expect("the disposable server starts");
    let display = DisplayServer::start("x11-badxauth").expect("the display server starts");
    let broken = display.broken_xauth_path();
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&broken),
    );

    let mut session = connect(&server, X11Forwarding::Restricted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("no restricted X11 cookie"),
                "the reason names the credential that could not be made: {reason}"
            );
            assert!(
                reason.contains("untrusted cookie"),
                "the reason repeats what xauth said: {reason}"
            );
            assert!(
                next_step.to_lowercase().contains("xauth")
                    && next_step.to_lowercase().contains("trusted"),
                "the next step names both ways out: {next_step}"
            );
        }
        other => panic!("a failing xauth cannot restrict, got {other:?}"),
    }
    assert_eq!(
        display.connections().unwrap_or(0),
        0,
        "the trusted cookie must not have been used instead"
    );
    write(&mut session, "echo STILL-$((2 + 40))");
    let output = wait_for_output(&mut session, "STILL-42", &started.output);
    assert!(output.contains("STILL-42"), "{output:?}");
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
    // The trusted mode, because the server is what this test is about: the restricted
    // mode asks this machine's display for a restricted cookie first, and that check
    // comes before the request is sent (this host's Xvfb refuses the cookies it issues).
    // A refusal of the request itself is what is measured here, and the trusted mode is
    // the one that reaches the server on every display.
    let broken = display.broken_xauth_path();
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&broken),
    );

    let mut session = connect(&server, X11Forwarding::Trusted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("refused"),
                "a refusal is described as one: {reason}"
            );
            // The server has xauth, so the refusal is its forwarding configuration; the
            // reason says that instead of listing both server-side causes.
            assert!(
                reason.contains("X11Forwarding") && reason.contains("xauth"),
                "the reason is the one the server's own state proves: {reason}"
            );
            assert!(
                next_step.contains("X11Forwarding") && next_step.contains("xauth"),
                "the next step names the configuration to change: {next_step}"
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
    // The trusted mode again: the server's own preparation is the subject, and this
    // host's display server refuses the restricted cookies it issues, which the product
    // reports before any request is sent.
    let broken = display.broken_xauth_path();
    let _env = DisplayEnv::set(
        Some(&display.display()),
        Some(&display.authority()),
        Some(&broken),
    );

    let mut session = connect(&server, X11Forwarding::Trusted);
    let started = wait_connected(&mut session);
    match wait_x11(&mut session, started.x11) {
        X11Status::Failed { reason, next_step } => {
            assert!(
                reason.contains("refused"),
                "the refusal is described as one: {reason}"
            );
            // The server's own check found no usable xauth, which is a different reason
            // from a server that forbids forwarding.
            assert!(
                reason.contains("no usable xauth"),
                "the reason is the cause the probe could prove: {reason}"
            );
            assert!(
                !reason.contains("X11Forwarding"),
                "a missing xauth is not reported as a forbidden server: {reason}"
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
