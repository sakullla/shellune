//! The RDP and VNC endpoint matrix (R18-R20, R23).
//!
//! This file is the one place a *real* server is asked for a picture. Its subject
//! is the set of endpoints a release has to be shown against, and its honesty
//! condition is the important part: an endpoint that is not configured here is
//! reported as unverified, never as passed.
//!
//! The protocol boundary itself - the ABI, cancellation, the close order, the
//! failure classification, a first frame with input - is proven against a scripted
//! peer in tests/desktop_native.rs. That is not evidence that any real server
//! works; this file is, and on a machine with no server it says so.
//!
//! Configuring an endpoint (all optional, none with a default):
//!
//! * SHELLUNE_RDP_WINDOWS_HOST / _PORT / _USER / _PASSWORD - a Windows host
//!   offering Remote Desktop;
//! * SHELLUNE_RDP_XRDP_HOST / _PORT / _USER / _PASSWORD - an xrdp server;
//! * SHELLUNE_VNC_HOST / _PORT / _PASSWORD - a VNC server using None or VncAuth;
//! * SHELLUNE_VNC_PASSWORD_HOST - a second VNC server (VncAuth) so the two-server
//!   requirement is met;
//! * SHELLUNE_VNC_TLS_HOST / _PORT / _PASSWORD - a server offering TLS or
//!   VeNCrypt (a certificate is presented for the user to accept);
//! * SHELLUNE_VNC_BAD_PASSWORD - the password to offer the server above to make it
//!   refuse the credentials.
//!
//! When a variable is missing the endpoint is listed with the reason its
//! verification did not run. Nothing in this file is skipped silently.

use std::time::{Duration, Instant};

use shellune_desktop::{
    DesktopClient, Error, Event, Failure, PointerButtons, Protocol, State, VncSecurity,
    backend_info,
};

/// How long one endpoint may take to answer before the report calls it a hang.
const PATIENCE: Duration = Duration::from_secs(20);

/// One endpoint the release has to be shown against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A Windows host with Remote Desktop.
    WindowsRdp,
    /// An xrdp server that must not present a refused password as a session.
    XrdpWrongPassword,
    /// An xrdp server.
    Xrdp,
    /// A VNC server that uses None or VncAuth.
    Vnc,
    /// A second VNC server, so two independent implementations are covered.
    VncSecond,
    /// A VNC server offering TLS or VeNCrypt.
    VncTls,
    /// A VNC server that must refuse a deliberately wrong password.
    VncWrongPassword,
}

impl Kind {
    /// Every endpoint this matrix requires, in the order the report lists them.
    const ALL: [Kind; 7] = [
        Kind::WindowsRdp,
        Kind::Xrdp,
        Kind::XrdpWrongPassword,
        Kind::Vnc,
        Kind::VncSecond,
        Kind::VncTls,
        Kind::VncWrongPassword,
    ];

    /// The name the report uses.
    fn label(self) -> &'static str {
        match self {
            Self::WindowsRdp => "RDP on Windows",
            Self::Xrdp => "RDP on xrdp",
            Self::XrdpWrongPassword => "RDP with a refused password",
            Self::Vnc => "VNC (None or VncAuth)",
            Self::VncSecond => "VNC (second server)",
            Self::VncTls => "VNC over TLS or VeNCrypt",
            Self::VncWrongPassword => "VNC with a refused password",
        }
    }

    /// The protocol the endpoint speaks.
    fn protocol(self) -> Protocol {
        match self {
            Self::WindowsRdp | Self::Xrdp | Self::XrdpWrongPassword => Protocol::Rdp,
            _ => Protocol::Vnc,
        }
    }

    /// The variables that configure it: host, port, user, password.
    fn variables(
        self,
    ) -> (
        &'static str,
        &'static str,
        Option<&'static str>,
        &'static str,
    ) {
        match self {
            Self::WindowsRdp => (
                "SHELLUNE_RDP_WINDOWS_HOST",
                "SHELLUNE_RDP_WINDOWS_PORT",
                Some("SHELLUNE_RDP_WINDOWS_USER"),
                "SHELLUNE_RDP_WINDOWS_PASSWORD",
            ),
            Self::Xrdp => (
                "SHELLUNE_RDP_XRDP_HOST",
                "SHELLUNE_RDP_XRDP_PORT",
                Some("SHELLUNE_RDP_XRDP_USER"),
                "SHELLUNE_RDP_XRDP_PASSWORD",
            ),
            Self::XrdpWrongPassword => (
                "SHELLUNE_RDP_XRDP_HOST",
                "SHELLUNE_RDP_XRDP_PORT",
                Some("SHELLUNE_RDP_XRDP_USER"),
                "SHELLUNE_RDP_XRDP_BAD_PASSWORD",
            ),
            Self::Vnc => (
                "SHELLUNE_VNC_HOST",
                "SHELLUNE_VNC_PORT",
                None,
                "SHELLUNE_VNC_PASSWORD",
            ),
            Self::VncSecond => (
                "SHELLUNE_VNC_PASSWORD_HOST",
                "SHELLUNE_VNC_PASSWORD_PORT",
                None,
                "SHELLUNE_VNC_PASSWORD_PASSWORD",
            ),
            Self::VncTls => (
                "SHELLUNE_VNC_TLS_HOST",
                "SHELLUNE_VNC_TLS_PORT",
                None,
                "SHELLUNE_VNC_TLS_PASSWORD",
            ),
            Self::VncWrongPassword => (
                "SHELLUNE_VNC_HOST",
                "SHELLUNE_VNC_PORT",
                None,
                "SHELLUNE_VNC_BAD_PASSWORD",
            ),
        }
    }

    /// Whether this endpoint is expected to be refused.
    fn expects_refusal(self) -> bool {
        matches!(self, Self::VncWrongPassword | Self::XrdpWrongPassword)
    }
}

/// One endpoint as the environment describes it.
struct Endpoint {
    kind: Kind,
    address: String,
    port: u16,
    user: Option<String>,
    password: Option<String>,
}

impl Endpoint {
    /// The endpoint the environment describes, or why nothing was configured.
    fn from_env(kind: Kind) -> Result<Self, String> {
        let (host_var, port_var, user_var, password_var) = kind.variables();
        let Some(address) = std::env::var(host_var)
            .ok()
            .filter(|value| !value.is_empty())
        else {
            return Err(format!("{host_var} is not set"));
        };
        let port = match std::env::var(port_var)
            .ok()
            .filter(|value| !value.is_empty())
        {
            Some(text) => text
                .parse::<u16>()
                .map_err(|error| format!("{port_var} is not a port: {error}"))?,
            None => {
                if kind.protocol() == Protocol::Rdp {
                    3389
                } else {
                    5900
                }
            }
        };
        let user = user_var
            .and_then(|name| std::env::var(name).ok())
            .filter(|value| !value.is_empty());
        let password = std::env::var(password_var)
            .ok()
            .filter(|value| !value.is_empty());
        Ok(Self {
            kind,
            address,
            port,
            user,
            password,
        })
    }

    /// What this endpoint may ask for.
    fn security(&self) -> VncSecurity {
        match self.kind {
            Kind::VncTls => VncSecurity::TLS.with(VncSecurity::VENCRYPT),
            _ => VncSecurity::DEFAULT,
        }
    }
}

/// What one endpoint reported.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The run reached the live state and every step it tests.
    Verified(Vec<(&'static str, bool)>),
    /// This build has no backend for the protocol, so nothing was proven.
    Unbuilt(String),
    /// The endpoint answered but the run did not prove what it was there to prove.
    ///
    /// Kept apart from [Self::Failed] so the report can say "not verified, and here
    /// is why" instead of "wrong": a server that offers no security type this build
    /// can negotiate proved nothing about encryption, and calling that a pass would
    /// be the one thing this file must never do.
    Unverified(String),
    /// The endpoint answered, and the answer was not the one required.
    Failed(String),
}

/// How long a session has to keep running without a failure before the endpoint
/// counts as verified: a picture alone is not a working session, as a server that
/// drops the connection after a few seconds would still deliver one.
const SUSTAIN: Duration = Duration::from_secs(10);

/// Runs the matrix for one endpoint.
fn verify(endpoint: &Endpoint) -> Outcome {
    if let Err(Error::Unavailable(reason)) = backend_info(endpoint.kind.protocol()) {
        return Outcome::Unbuilt(reason);
    }
    let mut client = match DesktopClient::new(endpoint.kind.protocol()) {
        Ok(client) => client,
        Err(Error::Unavailable(reason)) => return Outcome::Unbuilt(reason),
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    if let Err(error) = client.set_server(&endpoint.address, endpoint.port) {
        return Outcome::Failed(format!("set_server: {error}"));
    }
    if let Err(error) =
        client.set_credentials(endpoint.user.as_deref(), endpoint.password.as_deref(), None)
    {
        return Outcome::Failed(format!("set_credentials: {error}"));
    }
    if endpoint.kind.protocol() == Protocol::Vnc {
        if let Err(error) = client.set_vnc_security(endpoint.security()) {
            return Outcome::Failed(format!("set_vnc_security: {error}"));
        }
        // Four bytes per pixel is the only depth the shim delivers; asking for it
        // explicitly is what makes the frames comparable with what the pane draws.
        if let Err(error) = client.set_vnc_colour_depth(4) {
            return Outcome::Failed(format!("set_vnc_colour_depth: {error}"));
        }
    }
    if let Err(error) = client.set_size(1280, 720) {
        return Outcome::Failed(format!("set_size: {error}"));
    }
    if let Err(error) = client.connect() {
        return Outcome::Failed(format!("connect: {error}"));
    }

    let mut steps: Vec<(&'static str, bool)> = Vec::new();
    let mut certificate_asked = false;
    // Whether every certificate question the server asked was answered, and whether
    // the answer went through. A step that is never taken is not a step that passed:
    // a server that asks for nothing has no answer to prove.
    let mut certificate_answer_failed = false;
    let mut frames = 0usize;
    let mut first_frame = false;
    let mut failure: Option<(Failure, String)> = None;

    // One step of the session, shared by both phases below: certificates are
    // answered, frames counted, and a failure remembered.
    let step = |client: &DesktopClient,
                event: Event,
                frames: &mut usize,
                first_frame: &mut bool,
                failure: &mut Option<(Failure, String)>,
                certificate_asked: &mut bool,
                certificate_answer_failed: &mut bool| {
        match event {
            Event::Certificate { request_id, .. } => {
                *certificate_asked = true;
                // The matrix trusts the certificate it was pointed at: the decision
                // itself is a user's, and the pane asks for it. An answer that does
                // not go through leaves the server waiting, which is a failed run
                // and not a silent one.
                if client.answer_certificate(request_id, true).is_err() {
                    *certificate_answer_failed = true;
                }
            }
            Event::Frame { width, height, .. } => {
                if width > 0 && height > 0 {
                    *frames += 1;
                    *first_frame = true;
                }
            }
            Event::State {
                state,
                failure: reported,
                reason,
                ..
            } => match state {
                State::Connected => {}
                State::Failed(_) | State::Closed => *failure = Some((reported, reason)),
                _ => {}
            },
            Event::Clipboard { .. } | Event::Notice { .. } => {}
        }
    };

    // Phase one: the first picture, or the refusal this endpoint exists to show.
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && failure.is_none() && !first_frame {
        if let Ok(event) = client.recv_timeout(Duration::from_millis(100)) {
            step(
                &client,
                event,
                &mut frames,
                &mut first_frame,
                &mut failure,
                &mut certificate_asked,
                &mut certificate_answer_failed,
            );
        }
    }
    if let Some((reported, reason)) = failure.take() {
        if endpoint.kind.expects_refusal() {
            return refusal_outcome(endpoint, reported, &reason);
        }
        return match reported {
            // The TLS endpoint exists to prove that an encrypted VNC session can be
            // opened. A server that answers with a security type this build cannot
            // negotiate never opened one.
            Failure::UnsupportedSecurity if endpoint.kind == Kind::VncTls => Outcome::Unverified(
                "the server offered no security type this build can negotiate, so no TLS or VeNCrypt session was opened and encryption was not verified".to_owned(),
            ),
            other => Outcome::Failed(format!(
                "{} reported {other:?} before the session was live ({reason})",
                endpoint.kind.label()
            )),
        };
    }
    if !first_frame {
        steps.push(("first frame", false));
        return Outcome::Failed("the session connected but drew no frame".to_owned());
    }
    // A session that comes up with a password the server was supposed to refuse is
    // exactly the silent pass this matrix must never produce.
    if endpoint.kind.expects_refusal() {
        return match endpoint.kind {
            Kind::XrdpWrongPassword => Outcome::Failed(
                "the session came up with a deliberately wrong password: this server \
                 refuses credentials only on its own screen, so the client cannot tell \
                 a refused logon from an accepted one here"
                    .to_owned(),
            ),
            _ => Outcome::Failed("the server accepted a deliberately wrong password".to_owned()),
        };
    }

    // Phase two: the session has to stay up. A connection that drops mid-paint, or
    // that stops delivering updates, is not a working desktop.
    let until = Instant::now() + SUSTAIN;
    while Instant::now() < until && failure.is_none() {
        if let Ok(event) = client.recv_timeout(Duration::from_millis(100)) {
            step(
                &client,
                event,
                &mut frames,
                &mut first_frame,
                &mut failure,
                &mut certificate_asked,
                &mut certificate_answer_failed,
            );
        }
    }
    if let Some((reported, reason)) = failure {
        return Outcome::Failed(format!(
            "the session reported {reported:?} after {} frames and {:.1}s ({reason})",
            frames,
            SUSTAIN.as_secs_f32()
        ));
    }

    steps.push(("first frame", true));
    // Sustained: the session survived the whole window above, and its worker is
    // still running at the end of it. Repaints are not part of this check, since a
    // desktop that does not change sends no updates.
    steps.push((
        "sustained",
        matches!(
            client.wait_for_worker(Duration::from_millis(1)),
            Err(Error::Timeout)
        ),
    ));
    if certificate_asked {
        steps.push(("certificate answered", !certificate_answer_failed));
    }
    // Logon confirmation is not a step here: the client reports Connected with a
    // reason when the server shows its verdict only on its own screen (xrdp
    // without CredSSP), and a refused logon is already a failure before this point.
    // Input: one key press and one pointer move at the top-left corner, which every
    // desktop has room for.
    let key = client.key(0x1e, 0x61, true, false);
    let release = client.key(0x1e, 0x61, false, false);
    let pointer = client.pointer(1, 1, PointerButtons(0).with(PointerButtons::LEFT));
    let moved = client.pointer(2, 2, PointerButtons(0));
    steps.push((
        "input",
        key.is_ok() && release.is_ok() && pointer.is_ok() && moved.is_ok(),
    ));
    // Both backends share the clipboard now: RDP through cliprdr, VNC through the
    // (extended) cut text.
    let shared = client.clipboard("shellune endpoint matrix");
    steps.push(("clipboard", shared.is_ok()));
    let cancelled = client.cancel().is_ok();
    let stopped = client.wait_for_worker(PATIENCE).is_ok();
    steps.push(("cancel", cancelled && stopped));
    Outcome::Verified(steps)
}

/// A real VNC endpoint serves a second session after the first one is closed, so
/// the application can reconnect. The matrix runs each endpoint once; this proves
/// the second attempt is a new, working session and not a cached picture.
#[test]
fn the_vnc_endpoint_accepts_a_second_session() {
    let endpoint = match Endpoint::from_env(Kind::Vnc) {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            println!("VNC reconnect: not verified ({reason})");
            return;
        }
    };
    if let Err(Error::Unavailable(reason)) = backend_info(Protocol::Vnc) {
        println!("VNC reconnect: not verified (this build has no VNC backend: {reason})");
        return;
    }
    for attempt in 1..=2 {
        let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
        client
            .set_server(&endpoint.address, endpoint.port)
            .expect("server");
        client
            .set_credentials(None, endpoint.password.as_deref(), None)
            .expect("credentials");
        client
            .set_vnc_security(endpoint.security())
            .expect("security");
        client.set_vnc_colour_depth(4).expect("depth");
        client.set_size(1280, 720).expect("size");
        client.connect().expect("connect");

        let deadline = Instant::now() + PATIENCE;
        let mut first_frame = false;
        let mut failure = None;
        while Instant::now() < deadline && !first_frame && failure.is_none() {
            match client.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Frame { width, height, .. }) if width > 0 && height > 0 => {
                    first_frame = true;
                    println!("VNC reconnect attempt {attempt}: {width}x{height} frame");
                }
                Ok(Event::Certificate { request_id, .. }) => {
                    client.answer_certificate(request_id, true).expect("answer");
                }
                Ok(Event::State {
                    state: State::Failed(_) | State::Closed,
                    failure: reported,
                    reason,
                    ..
                }) => failure = Some((reported, reason)),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(first_frame, "attempt {attempt} drew no frame: {failure:?}");
        assert!(client.cancel().is_ok());
        client.wait_for_worker(PATIENCE).expect("the worker stops");
    }
}

/// Drives the endpoint's pointer and keyboard, and offers text on its clipboard.
/// The observable result is on the server's own screen: after this runs, the
/// pointer is where it was sent and the right click has left its menu open. The
/// test asserts the protocol steps were accepted while the session stayed live.
#[test]
fn the_vnc_endpoint_takes_pointer_keyboard_and_clipboard() {
    let endpoint = match Endpoint::from_env(Kind::Vnc) {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            println!("VNC input: not verified ({reason})");
            return;
        }
    };
    if let Err(Error::Unavailable(reason)) = backend_info(Protocol::Vnc) {
        println!("VNC input: not verified (this build has no VNC backend: {reason})");
        return;
    }
    let mut client = DesktopClient::new(Protocol::Vnc).expect("client");
    client
        .set_server(&endpoint.address, endpoint.port)
        .expect("server");
    client
        .set_credentials(None, endpoint.password.as_deref(), None)
        .expect("credentials");
    client
        .set_vnc_security(endpoint.security())
        .expect("security");
    client.set_vnc_colour_depth(4).expect("depth");
    client.set_size(1280, 720).expect("size");
    client.connect().expect("connect");

    let deadline = Instant::now() + PATIENCE;
    let mut live = false;
    while Instant::now() < deadline && !live {
        match client.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Frame { .. })
            | Ok(Event::State {
                state: State::Connected,
                ..
            }) => {
                live = true;
            }
            Ok(Event::Certificate { request_id, .. }) => {
                client.answer_certificate(request_id, true).expect("answer");
            }
            Ok(Event::State {
                state: State::Failed(failure),
                reason,
                ..
            }) => panic!("the session failed: {failure:?} {reason}"),
            Ok(_) | Err(_) => {}
        }
    }
    assert!(live, "the endpoint never became live");

    // The pointer, a right click (a menu opens on the desktop) and a key press.
    client.pointer(400, 300, PointerButtons(0)).expect("move");
    client
        .pointer(400, 300, PointerButtons::RIGHT)
        .expect("right press");
    client
        .pointer(400, 300, PointerButtons(0))
        .expect("release");
    client.key(0x1e, 0x61, true, false).expect("key down");
    client.key(0x1e, 0x61, false, false).expect("key up");
    client
        .clipboard("shellune endpoint text")
        .expect("clipboard");
    std::thread::sleep(Duration::from_millis(800));
    println!(
        "VNC input: pointer 400,300 and a right click were sent, plus one key and one clipboard text"
    );
    // The session must still be live after the input, not have failed on it.
    let after = client.recv_timeout(Duration::from_millis(500));
    assert!(
        !matches!(
            after,
            Ok(Event::State {
                state: State::Failed(_) | State::Closed,
                ..
            })
        ),
        "the session ended on the input: {after:?}"
    );
    client.cancel().expect("cancel");
    client.wait_for_worker(PATIENCE).expect("the worker stops");
}

/// What a deliberately wrong password has to produce.
fn refusal_outcome(endpoint: &Endpoint, reported: Failure, reason: &str) -> Outcome {
    match endpoint.kind {
        Kind::VncWrongPassword => {
            if reported == Failure::Authentication {
                Outcome::Verified(vec![("refused", true)])
            } else {
                Outcome::Failed(format!(
                    "the wrong password was reported as {reported:?}, not as a refused credential"
                ))
            }
        }
        // An RDP server that does not negotiate CredSSP cannot refuse a password at
        // the protocol level: xrdp draws its own login screen instead. What must
        // never happen is a session that looks logged on.
        _ => {
            if reported == Failure::Authentication {
                Outcome::Verified(vec![("refused", true)])
            } else {
                Outcome::Unverified(format!(
                    "the wrong password was reported as {reported:?} ({reason}): this server                      refuses credentials only on its own screen, not in the protocol"
                ))
            }
        }
    }
}

/// The matrix, as one report.
#[test]
fn the_endpoint_matrix_reports_every_endpoint_it_could_not_verify() {
    let mut verified = Vec::new();
    let mut unverified = Vec::new();
    for kind in Kind::ALL {
        match Endpoint::from_env(kind) {
            Err(reason) => unverified.push(format!("{}: not verified ({reason})", kind.label())),
            Ok(endpoint) => match verify(&endpoint) {
                Outcome::Verified(steps) => {
                    let failed: Vec<&str> = steps
                        .iter()
                        .filter(|(_, ok)| !ok)
                        .map(|(name, _)| *name)
                        .collect();
                    if failed.is_empty() {
                        verified.push(format!("{}: verified", kind.label()));
                    } else {
                        unverified.push(format!(
                            "{}: failed at {}",
                            kind.label(),
                            failed.join(", ")
                        ));
                    }
                }
                Outcome::Unbuilt(reason) => unverified.push(format!(
                    "{}: not verified (this build has no {} backend: {reason})",
                    kind.label(),
                    match kind.protocol() {
                        Protocol::Rdp => "RDP",
                        Protocol::Vnc => "VNC",
                    }
                )),
                Outcome::Unverified(reason) => {
                    unverified.push(format!("{}: not verified ({reason})", kind.label()));
                }
                Outcome::Failed(reason) => unverified.push(format!("{}: {reason}", kind.label())),
            },
        }
    }

    // The report is the deliverable. A machine with no servers prints every
    // endpoint with the variable that would configure it, and nothing is ever
    // counted as verified without a server having answered.
    println!("desktop endpoint matrix (R18-R20, R23):");
    for line in &verified {
        println!("  VERIFIED  {line}");
    }
    for line in &unverified {
        println!("  UNVERIFIED  {line}");
    }
    assert_eq!(
        verified.len() + unverified.len(),
        Kind::ALL.len(),
        "every endpoint is reported exactly once"
    );
    for kind in Kind::ALL {
        let name = kind.label();
        assert!(
            verified
                .iter()
                .chain(unverified.iter())
                .any(|line| line.starts_with(name)),
            "{name} is missing from the report"
        );
    }
}

#[test]
fn an_unencrypted_vnc_policy_can_never_be_described_as_secure() {
    // The mask a connection gets when the user leaves the security at the server's
    // default. If it contained TLS or VeNCrypt this test would be wrong; the point
    // is that it does not, so a session using it is described as unencrypted.
    let default_mask = VncSecurity::DEFAULT;
    assert!(default_mask.contains(VncSecurity::NONE));
    assert!(default_mask.contains(VncSecurity::VNC_AUTH));
    assert!(
        !default_mask.contains(VncSecurity::TLS),
        "the default must not claim a TLS type it may not negotiate"
    );
    assert!(
        !default_mask.contains(VncSecurity::VENCRYPT),
        "the default must not claim VeNCrypt"
    );
    // Asking for encryption is a different policy, and it is one the connection has
    // to choose: nothing turns it on by accident.
    let encrypted = VncSecurity::TLS.with(VncSecurity::VENCRYPT);
    assert!(!encrypted.contains(VncSecurity::NONE));
    assert!(!encrypted.contains(VncSecurity::VNC_AUTH));
}

#[test]
fn a_protocol_without_a_linked_backend_says_so_instead_of_connecting() {
    for protocol in [Protocol::Rdp, Protocol::Vnc] {
        match backend_info(protocol) {
            Ok(info) => assert!(!info.is_empty(), "{protocol:?} reported an empty build"),
            Err(Error::Unavailable(reason)) => {
                assert!(!reason.is_empty());
                // The same answer the pane shows: this build cannot open it.
                assert!(matches!(
                    DesktopClient::new(protocol),
                    Err(Error::Unavailable(_))
                ));
            }
            Err(error) => panic!("{protocol:?} reported {error}"),
        }
    }
}
