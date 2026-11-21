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
//! * SHELLUNE_VNC_TLS_HOST / _PORT / _PASSWORD - a server offering certificate
//!   TLS or VeNCrypt (a certificate is presented for the user to accept);
//! * SHELLUNE_VNC_MIXED_HOST / _PORT / _PASSWORD - a server offering anonymous
//!   TLS and certificate TLS together, so the certificate form has to be chosen;
//! * SHELLUNE_VNC_ANON_HOST / _PORT - a server offering only anonymous TLS, which
//!   this build has to refuse with UnsupportedSecurity;
//! * SHELLUNE_VNC_OBSERVE (an ssh target), _USER, _DISPLAY and _XAUTHORITY - the
//!   machine and X session whose pointer, keys and clipboard are read back with
//!   xdotool, xterm and xclip;
//! * SHELLUNE_DESKTOP_TEST_TIMEOUT_SECS - optional first-picture deadline (default 20);
//! * SHELLUNE_VNC_BAD_PASSWORD - the password to offer the server above to make it
//!   refuse the credentials.
//!
//! When a variable is missing the endpoint is listed with the reason its
//! verification did not run. Nothing in this file is skipped silently.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use shellune_desktop::{
    DesktopClient, Error, Event, Failure, PointerButtons, Protocol, State, VncSecurity,
    backend_info,
};

/// How long one endpoint may take to answer before the report calls it a hang.
fn first_picture_timeout() -> Duration {
    let seconds = std::env::var("SHELLUNE_DESKTOP_TEST_TIMEOUT_SECS")
        .map(|text| {
            text.parse::<u32>()
                .expect("first-picture timeout in seconds")
        })
        .unwrap_or(20);
    Duration::from_secs(u64::from(seconds))
}

const CLOSE_TIMEOUT: Duration = Duration::from_secs(20);

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
    /// A VNC server offering certificate TLS or VeNCrypt.
    VncTls,
    /// A VNC server that offers anonymous TLS and certificate TLS together; the
    /// certificate form must be chosen.
    VncMixed,
    /// A VNC server that offers only anonymous TLS, which this build must refuse
    /// with a readable reason instead of failing inside a TLS handshake.
    VncAnonymousTls,
    /// A VNC server that must refuse a deliberately wrong password.
    VncWrongPassword,
}

impl Kind {
    /// Every endpoint this matrix requires, in the order the report lists them.
    const ALL: [Kind; 9] = [
        Kind::WindowsRdp,
        Kind::Xrdp,
        Kind::XrdpWrongPassword,
        Kind::Vnc,
        Kind::VncSecond,
        Kind::VncTls,
        Kind::VncMixed,
        Kind::VncAnonymousTls,
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
            Self::VncMixed => "VNC with anonymous and certificate TLS",
            Self::VncAnonymousTls => "VNC with only anonymous TLS",
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
            Self::VncMixed => (
                "SHELLUNE_VNC_MIXED_HOST",
                "SHELLUNE_VNC_MIXED_PORT",
                None,
                "SHELLUNE_VNC_MIXED_PASSWORD",
            ),
            Self::VncAnonymousTls => (
                "SHELLUNE_VNC_ANON_HOST",
                "SHELLUNE_VNC_ANON_PORT",
                None,
                "SHELLUNE_VNC_ANON_PASSWORD",
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
        if kind.expects_refusal() && password.is_none() {
            return Err(format!("{password_var} is not set"));
        }
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
            // The encrypted endpoints answer through VeNCrypt; the standalone
            // anonymous TLS type is part of what the policy asks for, and the
            // anonymous endpoint exists to show it is refused.
            Kind::VncTls | Kind::VncMixed | Kind::VncAnonymousTls => {
                VncSecurity::TLS.with(VncSecurity::VENCRYPT)
            }
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

/// How much of the desktop the frames received so far have painted.
///
/// A first frame proves a working session only when the whole desktop is covered.
/// The first update of a live server usually arrives as several dirty regions, and
/// a 157x26 region is a picture of a corner, not of a desktop.
#[derive(Default)]
struct Coverage {
    width: u32,
    height: u32,
    painted: Vec<bool>,
    count: usize,
}

impl Coverage {
    /// Marks one dirty region; true when that made the coverage complete.
    fn add(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        desktop_width: u32,
        desktop_height: u32,
    ) -> bool {
        if width <= 0 || height <= 0 || desktop_width == 0 || desktop_height == 0 {
            return self.complete();
        }
        if self.width != desktop_width || self.height != desktop_height {
            self.width = desktop_width;
            self.height = desktop_height;
            self.painted = vec![false; (desktop_width as usize) * (desktop_height as usize)];
            self.count = 0;
        }
        let left = x.max(0) as u32;
        let top = y.max(0) as u32;
        let right = (x.max(0) as u32)
            .saturating_add(width as u32)
            .min(self.width);
        let bottom = (y.max(0) as u32)
            .saturating_add(height as u32)
            .min(self.height);
        for row in top..bottom {
            let start = (row as usize) * (self.width as usize);
            for column in left..right {
                let index = start + column as usize;
                if !self.painted[index] {
                    self.painted[index] = true;
                    self.count += 1;
                }
            }
        }
        self.complete()
    }

    fn complete(&self) -> bool {
        !self.painted.is_empty() && self.count == self.painted.len()
    }

    fn painted(&self) -> (usize, usize) {
        (self.count, self.painted.len())
    }
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
    let mut certificate_asked = false;
    // Whether every certificate question the server asked was answered, and whether
    // the answer went through. A step that is never taken is not a step that passed:
    // a server that asks for nothing has no answer to prove.
    let mut certificate_answer_failed = false;
    let mut frames = 0usize;
    let mut coverage = Coverage::default();
    let mut first_frame = false;
    // Only the VNC endpoints are judged by full coverage. An RDP desktop paints
    // through its own update stream, and the first frame is the same evidence it
    // has always been there.
    let covered = endpoint.kind.protocol() == Protocol::Vnc;
    let mut failure: Option<(Failure, String)> = None;

    // One step of the session, shared by both phases below: certificates are
    // answered, frames counted and their regions marked, and a failure remembered.
    let step = |client: &DesktopClient,
                event: Event,
                frames: &mut usize,
                coverage: &mut Coverage,
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
            Event::Frame {
                x,
                y,
                width,
                height,
                desktop_width,
                desktop_height,
                ..
            } => {
                if width > 0 && height > 0 {
                    *frames += 1;
                    *first_frame = if covered {
                        coverage.add(x, y, width, height, desktop_width, desktop_height)
                    } else {
                        true
                    };
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
            Event::Pointer(_) | Event::Clipboard { .. } | Event::Notice { .. } => {}
        }
    };

    // Phase one: the first picture, or the refusal this endpoint exists to show.
    let deadline = Instant::now() + first_picture_timeout();
    while Instant::now() < deadline && failure.is_none() && !first_frame {
        if let Ok(event) = client.recv_timeout(Duration::from_millis(100)) {
            step(
                &client,
                event,
                &mut frames,
                &mut coverage,
                &mut first_frame,
                &mut failure,
                &mut certificate_asked,
                &mut certificate_answer_failed,
            );
        }
    }
    if let Some((reported, reason)) = failure.take() {
        if endpoint.kind == Kind::VncAnonymousTls {
            return match reported {
                Failure::UnsupportedSecurity if reason.contains("anonymous TLS") => {
                    Outcome::Verified(vec![("anonymous TLS refused", true)])
                }
                Failure::UnsupportedSecurity => Outcome::Unverified(format!(
                    "the anonymous TLS endpoint was refused, but not for the anonymous TLS \
                     reason: {reason}"
                )),
                // A TLS alert or a closed socket here means the client tried the
                // handshake and failed inside it, which is exactly what must not
                // happen any more.
                other => Outcome::Failed(format!(
                    "the anonymous TLS endpoint failed as {other:?} instead of being refused \
                     up front ({reason})"
                )),
            };
        }
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
    if endpoint.kind == Kind::VncAnonymousTls {
        return Outcome::Failed(
            "the server accepted an anonymous TLS session this build is supposed to refuse"
                .to_owned(),
        );
    }
    if !first_frame {
        let (painted, total) = coverage.painted();
        steps.push(("first frame", false));
        return Outcome::Failed(if covered {
            format!("the session connected but painted {painted} of {total} desktop pixels")
        } else {
            "the session connected but drew no frame".to_owned()
        });
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
                &mut coverage,
                &mut first_frame,
                &mut failure,
                &mut certificate_asked,
                &mut certificate_answer_failed,
            );
        }
    }
    if let Some((reported, reason)) = failure {
        // The client does not dial again by itself any more, so a drop has to end
        // the session here - and it has to say why, because that sentence is what the
        // window shows instead of a second login on the server.
        if reason.trim().is_empty() {
            return Outcome::Failed(format!(
                "the session reported {reported:?} after {} frames and {:.1}s, with no \
                 reason to show",
                frames,
                SUSTAIN.as_secs_f32()
            ));
        }
        return Outcome::Failed(format!(
            "the session reported {reported:?} after {} frames and {:.1}s ({reason})",
            frames,
            SUSTAIN.as_secs_f32()
        ));
    }

    steps.push((
        if covered {
            "desktop covered"
        } else {
            "first frame"
        },
        true,
    ));
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
    let stopped = client.wait_for_worker(CLOSE_TIMEOUT).is_ok();
    steps.push(("cancel", cancelled && stopped));
    Outcome::Verified(steps)
}

/// A real VNC endpoint serves a second session after the first one is closed, so
/// the application can reconnect. The matrix runs each endpoint once; this proves
/// the second attempt is a new, working session and not a cached picture.
#[test]
fn the_vnc_endpoint_accepts_a_second_session() {
    let _sessions = vnc_sessions();
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

        let deadline = Instant::now() + first_picture_timeout();
        let mut coverage = Coverage::default();
        let mut first_frame = false;
        let mut failure = None;
        while Instant::now() < deadline && !first_frame && failure.is_none() {
            match client.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Frame {
                    x,
                    y,
                    width,
                    height,
                    desktop_width,
                    desktop_height,
                    ..
                }) if width > 0 && height > 0 => {
                    if coverage.add(x, y, width, height, desktop_width, desktop_height) {
                        first_frame = true;
                        println!(
                            "VNC reconnect attempt {attempt}: the whole {desktop_width}x{desktop_height} desktop was painted"
                        );
                    }
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
        let (painted, total) = coverage.painted();
        assert!(
            first_frame,
            "attempt {attempt} painted {painted} of {total} desktop pixels: {failure:?}"
        );
        assert!(client.cancel().is_ok());
        client
            .wait_for_worker(CLOSE_TIMEOUT)
            .expect("the worker stops");
    }
}

/// The real VNC tests share one server. Two viewers on one session would fight
/// over the clipboard and the pointer, so these tests take turns.
static VNC_SESSIONS: Mutex<()> = Mutex::new(());

fn vnc_sessions() -> std::sync::MutexGuard<'static, ()> {
    VNC_SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The machine the endpoint's desktop can be inspected on, reached over ssh.
///
/// Configured with `SHELLUNE_VNC_OBSERVE` (a user@host ssh target),
/// `SHELLUNE_VNC_OBSERVE_DISPLAY` (default `:21`) and optionally
/// `SHELLUNE_VNC_OBSERVE_XAUTHORITY`. The tools it runs are the ones that answer
/// the question directly: `xdotool` for the pointer, `xev` for key events and
/// `xclip` for the clipboard.
struct Observer {
    target: String,
    /// The user whose X session is inspected. The ssh login may be another user
    /// (root, for instance), and su is how the command reaches the session owner.
    user: Option<String>,
    display: String,
    authority: Option<String>,
}

impl Observer {
    fn from_env() -> Result<Observer, String> {
        let target = std::env::var("SHELLUNE_VNC_OBSERVE")
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "SHELLUNE_VNC_OBSERVE is not set".to_owned())?;
        let display = std::env::var("SHELLUNE_VNC_OBSERVE_DISPLAY")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| ":21".to_owned());
        let authority = std::env::var("SHELLUNE_VNC_OBSERVE_XAUTHORITY")
            .ok()
            .filter(|value| !value.is_empty());
        let user = std::env::var("SHELLUNE_VNC_OBSERVE_USER")
            .ok()
            .filter(|value| !value.is_empty());
        Ok(Observer {
            target,
            user,
            display,
            authority,
        })
    }

    fn run(&self, command: &str) -> Result<String, String> {
        let mut inner = format!("DISPLAY={} ", self.display);
        if let Some(authority) = &self.authority {
            inner.push_str(&format!("XAUTHORITY={authority} "));
        }
        inner.push_str(command);
        let shell = match &self.user {
            Some(user) => format!("su - {user} -c {}", quote(&inner)),
            None => inner,
        };
        let output = std::process::Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
            .arg(&self.target)
            .arg(shell)
            .output()
            .map_err(|error| format!("ssh could not run: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "{command}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// One shell word, quoted so a command with its own quotes survives su -c.
fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}
/// Drives the endpoint's pointer, keyboard and clipboard, and reads each one back
/// on the server's own desktop: `xdotool` reports the pointer where it was sent,
/// `xev` logs the key, and `xclip` returns the text the client offered. The
/// assertions are on those observations, not on the calls having returned.
#[test]
fn the_vnc_endpoint_takes_pointer_keyboard_and_clipboard() {
    let _sessions = vnc_sessions();
    let endpoint = match Endpoint::from_env(Kind::Vnc) {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            println!("VNC input: not verified ({reason})");
            return;
        }
    };
    let observer = match Observer::from_env() {
        Ok(observer) => observer,
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

    let deadline = Instant::now() + first_picture_timeout();
    let mut coverage = Coverage::default();
    let mut live = false;
    while Instant::now() < deadline && !live {
        match client.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Frame {
                x,
                y,
                width,
                height,
                desktop_width,
                desktop_height,
                ..
            }) if width > 0 && height > 0 => {
                live = coverage.add(x, y, width, height, desktop_width, desktop_height);
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
    assert!(live, "the endpoint never painted its desktop");

    // Keyboard: a terminal whose command is cat writes what it reads to a file, so
    // the key the client sent is read back as text. Any menu left open by an
    // earlier run holds a keyboard grab, so it is dismissed first.
    observer.run("xdotool key Escape").ok();
    observer
        .run("pkill -f '^xterm -T shellune-key-capture' || true; rm -f /tmp/shellune-key.txt")
        .unwrap_or_else(|error| panic!("xterm cleanup: {error}"));
    observer
        .run(
            "nohup xterm -T shellune-key-capture -geometry 40x5+100+100 \
             -e sh -c 'cat > /tmp/shellune-key.txt' >/dev/null 2>&1 </dev/null &",
        )
        .unwrap_or_else(|error| panic!("xterm start: {error}"));
    let mut window = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && window.is_empty() {
        if let Ok(found) = observer.run("xdotool search --name shellune-key-capture | head -1") {
            window = found.trim().to_owned();
        }
        if window.is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(
        !window.is_empty(),
        "xterm opened no window to receive the key"
    );
    observer
        .run(&format!("xdotool windowactivate --sync {window}"))
        .unwrap_or_else(|error| panic!("xterm focus: {error}"));
    // 'a' then Return: cat writes the line it read.
    client.key(0x1e, 0x61, true, false).expect("key down");
    client.key(0x1e, 0x61, false, false).expect("key up");
    client.key(0x1e, 0xff0d, true, false).expect("return down");
    client.key(0x1e, 0xff0d, false, false).expect("return up");
    let mut typed = String::new();
    let deadline = Instant::now() + first_picture_timeout();
    while Instant::now() < deadline && !typed.contains('a') {
        std::thread::sleep(Duration::from_millis(200));
        typed = observer
            .run("cat /tmp/shellune-key.txt")
            .unwrap_or_default();
    }
    let _ = observer.run("pkill -f '^xterm -T shellune-key-capture' || true");
    assert!(
        typed.contains('a'),
        "the terminal read {typed:?}, so the key press did not reach the desktop"
    );

    // Pointer: the server's own pointer must be where the client sent it.
    client.pointer(321, 234, PointerButtons(0)).expect("move");
    let deadline = Instant::now() + first_picture_timeout();
    let location = loop {
        let observed = observer
            .run("xdotool getmouselocation")
            .expect("read pointer");
        if observed.contains("x:321 y:234") || Instant::now() >= deadline {
            break observed;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(
        location.contains("x:321 y:234"),
        "the pointer is not where it was sent: {location}"
    );

    // Clipboard: the text the client offered is what the server's X server holds.
    let text = format!("shellune endpoint text {}", std::process::id());
    client.clipboard(&text).expect("clipboard");
    let mut clipboard = String::new();
    let deadline = Instant::now() + first_picture_timeout();
    while Instant::now() < deadline && clipboard.trim_end_matches('\n') != text {
        std::thread::sleep(Duration::from_millis(200));
        clipboard = observer
            .run("xclip -o -selection clipboard 2>/dev/null || true")
            .unwrap_or_else(|error| panic!("xclip: {error}"));
    }
    assert_eq!(
        clipboard.trim_end_matches('\n'),
        text,
        "the server's clipboard does not hold the offered text"
    );

    // A right click on the desktop; the session must still be live afterwards.
    client
        .pointer(400, 300, PointerButtons::RIGHT)
        .expect("right press");
    client
        .pointer(400, 300, PointerButtons(0))
        .expect("release");
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
    println!(
        "VNC input: the terminal read the key back, xdotool reports x:321 y:234, xclip holds the offered text"
    );
    client.cancel().expect("cancel");
    client
        .wait_for_worker(CLOSE_TIMEOUT)
        .expect("the worker stops");
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
    let _sessions = vnc_sessions();
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
