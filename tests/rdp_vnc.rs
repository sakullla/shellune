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
//!   VeNCrypt;
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
    const ALL: [Kind; 6] = [
        Kind::WindowsRdp,
        Kind::Xrdp,
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
            Self::Vnc => "VNC (None or VncAuth)",
            Self::VncSecond => "VNC (second server)",
            Self::VncTls => "VNC over TLS or VeNCrypt",
            Self::VncWrongPassword => "VNC with a refused password",
        }
    }

    /// The protocol the endpoint speaks.
    fn protocol(self) -> Protocol {
        match self {
            Self::WindowsRdp | Self::Xrdp => Protocol::Rdp,
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
        matches!(self, Self::VncWrongPassword)
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
    let mut failure: Option<Failure> = None;
    let mut first_frame = false;
    let mut certificate_asked = false;
    // Whether every certificate question the server asked was answered, and whether
    // the answer went through. A step that is never taken is not a step that passed:
    // a server that asks for nothing has no answer to prove.
    let mut certificate_answer_failed = false;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        match client.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => match event {
                Event::Certificate { request_id, .. } => {
                    certificate_asked = true;
                    // The matrix trusts the certificate it was pointed at: the
                    // decision itself is a user's, and the pane asks for it. An
                    // answer that does not go through leaves the server waiting,
                    // which is a failed run and not a silent one.
                    if client.answer_certificate(request_id, true).is_err() {
                        certificate_answer_failed = true;
                    }
                }
                Event::Frame { width, height, .. } => {
                    if width > 0 && height > 0 {
                        first_frame = true;
                        break;
                    }
                }
                Event::State {
                    state,
                    failure: reported,
                    ..
                } => match state {
                    // Connected is not a picture: keep waiting for the frame the
                    // matrix is there to see.
                    State::Connected => {}
                    State::Failed(_) | State::Closed => {
                        failure = Some(reported);
                        break;
                    }
                    _ => {}
                },
                Event::Clipboard { .. } | Event::Notice { .. } => {}
            },
            Err(_) => continue,
        }
    }
    if endpoint.kind.expects_refusal() {
        // For this endpoint a refusal is the required answer, so it is the one that
        // is reported as passed - with the step it happened on, so the run cannot
        // claim a sign-in it was meant to lose.
        steps.push(("refused", failure == Some(Failure::Authentication)));
        return match failure {
            Some(Failure::Authentication) => Outcome::Verified(steps),
            Some(other) => Outcome::Failed(format!(
                "the wrong password was reported as {other:?}, not as a refused credential"
            )),
            None => Outcome::Failed("the server accepted a deliberately wrong password".to_owned()),
        };
    }
    let Some(failure) = failure else {
        if !first_frame {
            // A connection without a picture is not a verified desktop: the matrix
            // is about what the user sees.
            steps.push(("first frame", false));
            return Outcome::Failed("the session connected but drew no frame".to_owned());
        }
        steps.push(("first frame", true));
        // One step per thing the run really did. The certificate step is only
        // present when a certificate was asked for, and it is true only if the
        // answer reached the server: an endpoint that asks for nothing does not get
        // a pass it never earned.
        if certificate_asked {
            steps.push(("certificate answered", !certificate_answer_failed));
        }
        // Input: one key press and one pointer move at the top-left corner, which
        // every desktop has room for.
        let key = client.key(0x1e, 0x61, true, false);
        let release = client.key(0x1e, 0x61, false, false);
        let pointer = client.pointer(1, 1, PointerButtons(0).with(PointerButtons::LEFT));
        let moved = client.pointer(2, 2, PointerButtons(0));
        steps.push((
            "input",
            key.is_ok() && release.is_ok() && pointer.is_ok() && moved.is_ok(),
        ));
        // Both backends share the clipboard now: RDP through cliprdr, VNC through
        // the (extended) cut text.
        let shared = client.clipboard("shellune endpoint matrix");
        steps.push(("clipboard", shared.is_ok()));
        let cancelled = client.cancel().is_ok();
        let stopped = client.wait_for_worker(PATIENCE).is_ok();
        steps.push(("cancel", cancelled && stopped));
        return Outcome::Verified(steps);
    };
    // A failure is the right answer only where it was the expected one.
    match failure {
        // The TLS endpoint exists to prove that an encrypted VNC session can be
        // opened. A server that answers with a security type this build cannot
        // negotiate never opened one, so the run proved nothing about encryption:
        // it is reported as not verified, never as passed.
        Failure::UnsupportedSecurity if endpoint.kind == Kind::VncTls => Outcome::Unverified(
            "the server offered no security type this build can negotiate, so no TLS or              VeNCrypt session was opened and encryption was not verified"
                .to_owned(),
        ),
        other => Outcome::Failed(format!(
            "{} reported {other:?} before the session was live",
            endpoint.kind.label()
        )),
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
