//! X11 forwarding for one remote session (R11).
//!
//! Forwarding is a bridge with two ends: the remote programs a server starts on behalf
//! of this session, and the X server of the machine the user is sitting at. This module
//! owns both ends and the rule that keeps them apart:
//!
//! * the local display is found from `DISPLAY` and its cookie from `XAUTHORITY`, so
//!   nothing has to be typed and no remote `DISPLAY` is ever set by hand;
//! * the cookie that travels to the server is a random one generated for this session.
//!   The local cookie never leaves this machine: it is written into each X11 connection
//!   setup request here, after the request arrived over SSH;
//! * the credential this bridge presents to the local display is the one the mode
//!   promises. [`X11Forwarding::Restricted`] has a cookie the display itself issued as
//!   *untrusted* (the same thing `xauth generate … untrusted` does for `ssh -X`), so
//!   the X11 SECURITY extension is what limits a remote program; that cookie is read
//!   from a private file of this session and the authority file of this machine is
//!   never written to;
//!   [`X11Forwarding::Trusted`] presents the display's own cookie and therefore lifts
//!   those limits, which is why the connection form asks again before each attempt;
//! * every inbound `x11` channel is bridged to the local display separately, so a
//!   second program in the same session needs no second authentication;
//! * closing the session first stops accepting channels and then drops the bridges, so
//!   nothing is left listening or forwarding afterwards.
//!
//! A machine without a reachable display server, a display whose authority file holds
//! no cookie, a display that cannot issue an untrusted cookie, and a server that
//! refuses the request are different failures: each names itself and its next step,
//! and none of them ends the SSH session, which keeps working without X11. A refusal
//! is asked about once, on the same authenticated connection, so a server without
//! `xauth` is told apart from a server that forbids forwarding.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use russh::client::{self, ChannelOpenHandle};
use russh::{Channel, ChannelMsg, ChannelOpenFailure};
use shellune_core::X11Forwarding;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

/// The only X11 authentication protocol this bridge speaks.
///
/// `MIT-MAGIC-COOKIE-1` is what `xauth` writes and what X servers ask for. An X11
/// setup request that names another protocol is forwarded unchanged: inventing a
/// translation for it would claim a support this bridge does not have.
pub const AUTH_NAME: &str = "MIT-MAGIC-COOKIE-1";

/// The port a TCP X display `:0` listens on; display `n` adds `n` to it.
const X11_TCP_BASE: u16 = 6000;

/// The largest setup request that is still treated as X11.
///
/// A setup request carries a protocol name and a cookie, both a few dozen bytes. The
/// cap keeps a misbehaving remote program from making this side buffer without bound.
const MAX_SETUP: usize = 64 * 1024;

/// How long the program that issues a restricted cookie may take.
///
/// It answers out of the local display, so it does not need a network round trip; the
/// limit is what keeps a wedged display server from holding a connection attempt open.
const XAUTH_PATIENCE: Duration = Duration::from_secs(10);

/// How long the server may take to say whether it has an `xauth` (R11).
///
/// The question is a courtesy that makes a refusal more precise; a server that does not
/// answer it in time leaves the refusal as the server stated it.
const PROBE_PATIENCE: Duration = Duration::from_secs(5);

/// Why X11 forwarding could not be used.
///
/// Each variant is a different thing to fix, which is why they are kept apart instead
/// of collapsing into one "X11 failed".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum X11Failure {
    /// `DISPLAY` is unset or does not name a display this machine could use.
    NoDisplay {
        /// What `DISPLAY` held, or that it held nothing.
        detail: String,
    },
    /// The display is named but no cookie for it was found.
    NoAuthority {
        /// Where the cookie was looked for and what was missing.
        detail: String,
    },
    /// The X server the display names could not be reached.
    DisplayUnreachable {
        /// The display that was tried.
        display: String,
        /// What the system said.
        detail: String,
    },
    /// The display answered, but could not issue an untrusted cookie.
    ///
    /// The restricted mode forwards with an authorization the X11 SECURITY extension
    /// treats as untrusted, which is the whole of what the mode promises. Forwarding
    /// with the display's own cookie instead would quietly give a remote program more
    /// than that, so a display that cannot issue one is a failure and not a fallback.
    NoUntrustedCookie {
        /// What went wrong while asking the display for one.
        detail: String,
    },
    /// The server refused the forwarding request and has no usable `xauth`.
    ///
    /// This is the server's own preparation failing: it cannot write the remote
    /// authority file, so it declines before `DISPLAY` is set.
    RemoteWithoutXauth {
        /// What the server's own check reported.
        detail: String,
    },
    /// The server refused the forwarding request although it has `xauth`.
    ///
    /// The usual cause is `X11Forwarding no`, and the other one is an `XAuthLocation`
    /// that names a program the server cannot run. The protocol carries no reason, so
    /// what is known is what the server's own check showed.
    ForwardingForbidden,
    /// The server refused the forwarding request and nothing more could be learned.
    Refused {
        /// What the request reported.
        reason: String,
    },
    /// The session ended, or the local display closed, while a channel was open.
    Lost {
        /// What the transport reported.
        detail: String,
    },
}

impl X11Failure {
    /// What the user can do about this failure.
    #[must_use]
    pub fn next_step(&self) -> String {
        match self {
            Self::NoDisplay { .. } => {
                "Start an X server for this computer and make sure DISPLAY names it, or connect without X11 forwarding."
                    .to_owned()
            }
            Self::NoAuthority { .. } => {
                "Add an MIT-MAGIC-COOKIE-1 entry for this display with xauth, or connect without X11 forwarding."
                    .to_owned()
            }
            Self::DisplayUnreachable { .. } => {
                "Start the X server the display names and try again, or connect without X11 forwarding."
                    .to_owned()
            }
            Self::NoUntrustedCookie { .. } => {
                "Install xauth so a restricted cookie can be created for this display, use a display server that supports the X11 SECURITY extension, or choose trusted forwarding and accept its risk. The SSH session itself is not affected."
                    .to_owned()
            }
            Self::RemoteWithoutXauth { .. } => {
                "Install xauth on the server (or point its XAuthLocation at one that runs), then connect again. The SSH session itself is not affected."
                    .to_owned()
            }
            Self::ForwardingForbidden => {
                "Turn on X11Forwarding and point XAuthLocation at a working xauth in the server's sshd_config, then connect again. The SSH session itself is not affected."
                    .to_owned()
            }
            Self::Refused { .. } => {
                "Turn on X11Forwarding and make sure xauth is installed on the server, then connect again. The SSH session itself is not affected."
                    .to_owned()
            }
            Self::Lost { .. } => {
                "Connect again to start a new X11 forwarding channel. The SSH session itself is not affected."
                    .to_owned()
            }
        }
    }
}

impl std::fmt::Display for X11Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDisplay { detail } => {
                write!(formatter, "no local X display is available: {detail}")
            }
            Self::NoAuthority { detail } => {
                write!(formatter, "no X11 authority cookie was found: {detail}")
            }
            Self::DisplayUnreachable { display, detail } => {
                write!(
                    formatter,
                    "the X server for display {display} is unreachable: {detail}"
                )
            }
            Self::NoUntrustedCookie { detail } => write!(
                formatter,
                "no restricted X11 cookie could be obtained: {detail}"
            ),
            Self::RemoteWithoutXauth { detail } => write!(
                formatter,
                "the server refused X11 forwarding: it has no usable xauth ({detail})"
            ),
            Self::ForwardingForbidden => formatter.write_str(
                "the server refused X11 forwarding although it has xauth: X11Forwarding is probably off, or XAuthLocation names a program that cannot run",
            ),
            Self::Refused { reason } => {
                write!(formatter, "the server refused X11 forwarding: {reason}")
            }
            Self::Lost { detail } => write!(formatter, "X11 forwarding stopped: {detail}"),
        }
    }
}

/// What a session's X11 forwarding is doing, for the session bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum X11Status {
    /// No forwarding was asked for. Nothing was requested and no `DISPLAY` is set.
    Off,
    /// The request is on its way and the server has not answered yet.
    Enabling,
    /// The server accepted the request; the display named here is bridged.
    Ready {
        /// The local display the session forwards to, as this machine names it.
        display: String,
    },
    /// Forwarding could not be used, with the reason and the next step.
    ///
    /// The SSH session is not affected: the terminal of a failed X11 attempt keeps
    /// working, which is why this is a status and not a session failure.
    Failed {
        /// What went wrong.
        reason: String,
        /// What to do about it.
        next_step: String,
    },
}

impl X11Status {
    /// Whether X11 forwarding is in use for this session.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// A failure value for a status that has one.
    fn failed(failure: &X11Failure) -> Self {
        Self::Failed {
            reason: failure.to_string(),
            next_step: failure.next_step(),
        }
    }
}

impl std::fmt::Display for X11Status {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => formatter.write_str("X11 forwarding is off"),
            Self::Enabling => formatter.write_str("X11 forwarding is being requested"),
            Self::Ready { display } => write!(formatter, "X11 forwarding is ready for {display}"),
            Self::Failed { reason, .. } => formatter.write_str(reason),
        }
    }
}

/// Where the local X server of one display listens.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DisplayTarget {
    /// A unix socket, either the one X servers use for a local display or the socket a
    /// launchd-named display points at.
    ///
    /// A platform without unix sockets cannot open one; the variant stays so the
    /// matches over a display name read the same way everywhere.
    #[allow(dead_code)]
    Unix {
        /// `/tmp/.X11-unix/X<n>`, or the path a display name gave.
        path: PathBuf,
        /// The same name as a Linux abstract socket, which some servers only have.
        abstract_name: String,
    },
    /// A TCP display, for X servers that listen on a port.
    Tcp {
        /// Address of the X server.
        host: String,
        /// `6000 + n`.
        port: u16,
    },
}

impl DisplayTarget {
    /// How the display is named in messages.
    fn describe(&self) -> String {
        match self {
            Self::Unix { path, .. } => path.display().to_string(),
            Self::Tcp { host, port } => format!("{host}:{port}"),
        }
    }
}

/// A display name, split into the parts forwarding needs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedDisplay {
    target: DisplayTarget,
    /// The display number, which the local X server also uses in its socket name.
    number: u32,
    /// The screen within that display.
    screen: u32,
    /// The display as this machine names it, for messages and the session bar.
    text: String,
    /// Whether the display is on this machine.
    local: bool,
}

/// Everything one session needs to bridge its X11 channels.
///
/// The credential stays in this value and is never formatted, logged or sent over the
/// connection; only [`Self::request_cookie`] travels to the server.
pub(crate) struct Prepared {
    target: DisplayTarget,
    screen: u32,
    display: String,
    /// The authority file this machine's own cookie was read from.
    ///
    /// A restricted session also hands it to the program that asks the display for an
    /// untrusted cookie, which needs it to reach the display once. It is only ever read
    /// from, both here and there: the entry the display issues goes into a private file.
    authority: PathBuf,
    /// The cookie this bridge presents to the local display.
    ///
    /// The display's own cookie in the trusted mode, and a cookie the display issued as
    /// untrusted in the restricted mode.
    credential: Vec<u8>,
    remote_cookie: String,
}

impl std::fmt::Debug for Prepared {
    /// Shows what was found, never the cookies themselves.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Prepared")
            .field("target", &self.target)
            .field("screen", &self.screen)
            .field("display", &self.display)
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl Prepared {
    /// The cookie the server is told to put in its `DISPLAY` authority file.
    ///
    /// A cookie of this session only: it authenticates the remote programs to this
    /// bridge, which then presents the local credential to the X server.
    pub(crate) fn request_cookie(&self) -> &str {
        &self.remote_cookie
    }
    /// The screen number the server should export.
    pub(crate) fn screen(&self) -> u32 {
        self.screen
    }

    /// Whether the X server this display names can be reached right now.
    ///
    /// Asked before the request is sent so a machine with no X server says so at once
    /// instead of failing quietly when the first remote program is started.
    pub(crate) async fn reachable(&self) -> Result<(), X11Failure> {
        match open_display(&self.target).await {
            Ok(_stream) => Ok(()),
            Err(error) => Err(X11Failure::DisplayUnreachable {
                display: self.display.clone(),
                detail: error.to_string(),
            }),
        }
    }

    /// Replaces the credential with an untrusted cookie of the local display.
    ///
    /// This is what makes the restricted mode restricted: a remote program reaches the
    /// display through an authorization the X server's SECURITY extension marks as
    /// untrusted, so it cannot use the parts of the display another application owns.
    /// When no such cookie can be had, this says so and keeps nothing: forwarding with
    /// the trusted cookie instead would be exactly the risk the mode promises to avoid.
    pub(crate) async fn restrict(&mut self, issue: CookieIssuer) -> Result<(), X11Failure> {
        let display = self.display.clone();
        let authority = self.authority.clone();
        // Registering the cookie means running a program that talks to the display,
        // which is blocking work and does not belong on the connection's thread.
        let credential = tokio::task::spawn_blocking(move || issue(&display, &authority))
            .await
            .map_err(|error| X11Failure::NoUntrustedCookie {
                detail: format!("the request for a restricted cookie could not be run: {error}"),
            })??;
        self.credential = credential;
        Ok(())
    }
}

/// What registers an untrusted cookie with the local display and returns it.
///
/// A function rather than a method so a test can drive the whole decision without an X
/// server: [`issue_untrusted_cookie`] is the real one.
pub(crate) type CookieIssuer = fn(&str, &Path) -> Result<Vec<u8>, X11Failure>;

/// Discovers the display and the cookie a session would forward to.
///
/// Reads only: nothing is connected to and no request is sent.
pub(crate) fn prepare(mode: X11Forwarding) -> Result<Prepared, X11Failure> {
    debug_assert!(mode.is_enabled());
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let authority = std::env::var_os("XAUTHORITY").map(PathBuf::from);
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    prepare_from(&display, authority.as_deref(), home.as_deref())
}

/// Everything one enabled session needs before its request is sent.
///
/// The two steps are in this order on purpose: a display that does not answer is
/// reported as unreachable before this machine tries to ask it for a restricted cookie,
/// and a display that cannot issue one is a failure of its own rather than a quiet
/// fallback to the trusted credential.
pub(crate) async fn arm(mode: X11Forwarding) -> Result<Prepared, X11Failure> {
    let mut prepared = prepare(mode)?;
    prepared.reachable().await?;
    if mode == X11Forwarding::Restricted {
        prepared.restrict(issue_untrusted_cookie).await?;
    }
    Ok(prepared)
}

/// The same, from values a test supplies instead of the environment.
fn prepare_from(
    display: &str,
    authority: Option<&Path>,
    home: Option<&Path>,
) -> Result<Prepared, X11Failure> {
    let parsed = parse_display(display)?;
    let (authority, credential) = read_cookie(&parsed, authority, home)?;
    // The session's own cookie: random, so a remote program can never authenticate to
    // the local X server with anything it was told, and so two sessions cannot use
    // each other's authority.
    let remote_cookie = hex(&uuid::Uuid::new_v4().into_bytes());
    Ok(Prepared {
        target: parsed.target,
        screen: parsed.screen,
        display: parsed.text,
        authority,
        credential,
        remote_cookie,
    })
}

/// Splits an X display name into target, number and screen.
fn parse_display(value: &str) -> Result<ParsedDisplay, X11Failure> {
    let text = value.trim();
    if text.is_empty() {
        return Err(X11Failure::NoDisplay {
            detail: "DISPLAY is not set".to_owned(),
        });
    }
    let Some((host, rest)) = text.rsplit_once(':') else {
        return Err(X11Failure::NoDisplay {
            detail: format!("{text} is not an X display name; expected [host]:display[.screen]"),
        });
    };
    let (number_text, screen_text) = match rest.split_once('.') {
        Some((number, screen)) => (number, Some(screen)),
        None => (rest, None),
    };
    let number: u32 = number_text.parse().map_err(|_| X11Failure::NoDisplay {
        detail: format!("{text} does not name a display number"),
    })?;
    let screen: u32 = match screen_text {
        Some(screen) => screen.parse().map_err(|_| X11Failure::NoDisplay {
            detail: format!("{text} does not name a screen number"),
        })?,
        None => 0,
    };
    let host = host.strip_prefix("tcp/").unwrap_or(host);
    // XQuartz names its display by the path of its launchd socket, for example
    // `/private/tmp/com.apple.launchd.XXXX/org.xquartz:0`. A host part with a path
    // separator in it is that socket and not a machine name, so the display is on this
    // computer even though the name does not look like `:0`.
    let socket_path = host.contains('/').then(|| host.to_owned());
    let local = host.is_empty()
        || host == "unix"
        || host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || socket_path.is_some();
    let target = if let Some(path) = socket_path {
        DisplayTarget::Unix {
            path: PathBuf::from(path.clone()),
            abstract_name: path,
        }
    } else if host.is_empty() || host == "unix" {
        local_target(number)?
    } else {
        let port = X11_TCP_BASE
            .checked_add(u16::try_from(number).map_err(|_| X11Failure::NoDisplay {
                detail: format!("{text} has a display number that is too large"),
            })?)
            .ok_or_else(|| X11Failure::NoDisplay {
                detail: format!("{text} has a display number that is too large"),
            })?;
        DisplayTarget::Tcp {
            host: host.to_owned(),
            port,
        }
    };
    Ok(ParsedDisplay {
        target,
        number,
        screen,
        text: text.to_owned(),
        local,
    })
}

/// Where the X server of a display on this machine listens.
///
/// Unix sockets are how an X server is reached on Linux and macOS; a Windows build has
/// no unix sockets for X, and the compatible servers there listen on a TCP port of the
/// loopback interface, which is what `:0` means on that platform.
fn local_target(number: u32) -> Result<DisplayTarget, X11Failure> {
    #[cfg(unix)]
    {
        let path = PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
        Ok(DisplayTarget::Unix {
            path,
            abstract_name: format!("/tmp/.X11-unix/X{number}"),
        })
    }
    #[cfg(not(unix))]
    {
        let port = X11_TCP_BASE
            .checked_add(u16::try_from(number).map_err(|_| X11Failure::NoDisplay {
                detail: format!("display :{number} is outside the range an X server uses"),
            })?)
            .ok_or_else(|| X11Failure::NoDisplay {
                detail: format!("display :{number} is outside the range an X server uses"),
            })?;
        Ok(DisplayTarget::Tcp {
            host: "127.0.0.1".to_owned(),
            port,
        })
    }
}

// ---------------------------------------------------------------------------
// Authority cookies
// ---------------------------------------------------------------------------

/// One record of an X authority file.
///
/// Visible in this crate so the end-to-end harness reads an authority file the same way
/// the product does, instead of a second parser that could drift from this one.
pub(crate) struct AuthorityEntry {
    pub(crate) family: u16,
    pub(crate) address: Vec<u8>,
    pub(crate) number: String,
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

/// Reads the cookie for a display out of the authority file it came from.
///
/// The file is returned with the cookie because the restricted mode needs it once more:
/// the program that issues an untrusted cookie has to reach the display with the
/// trusted one first.
fn read_cookie(
    parsed: &ParsedDisplay,
    authority: Option<&Path>,
    home: Option<&Path>,
) -> Result<(PathBuf, Vec<u8>), X11Failure> {
    let path = match authority {
        Some(path) => path.to_path_buf(),
        None => {
            let Some(home) = home else {
                return Err(X11Failure::NoAuthority {
                    detail: "XAUTHORITY and HOME are both unset, so no authority file can be found"
                        .to_owned(),
                });
            };
            home.join(".Xauthority")
        }
    };
    let bytes = std::fs::read(&path).map_err(|error| X11Failure::NoAuthority {
        detail: format!("{}: {error}", path.display()),
    })?;
    let entries = parse_authority(&bytes);
    let number = parsed.number.to_string();
    let cookie = find_cookie(&entries, &number, parsed).ok_or_else(|| X11Failure::NoAuthority {
        detail: format!(
            "{} holds no {AUTH_NAME} entry for display {}",
            path.display(),
            parsed.text
        ),
    })?;
    Ok((path, cookie))
}

/// Picks the cookie that belongs to a display.
///
/// A wildcard entry (`xauth add ... . ...`) applies to any display; an entry for this
/// display number wins over it. The address family decides whether an entry belongs to
/// this machine at all, which is why the display's own name is preferred over a local
/// entry when the display is reached over TCP.
fn find_cookie(
    entries: &[AuthorityEntry],
    number: &str,
    parsed: &ParsedDisplay,
) -> Option<Vec<u8>> {
    let mut best: Option<(u16, &AuthorityEntry)> = None;
    for entry in entries {
        if entry.name != AUTH_NAME {
            continue;
        }
        let number_score = if entry.number == number {
            2
        } else if entry.number.is_empty() {
            1
        } else {
            continue;
        };
        let address_score = match address_score(entry, parsed) {
            Some(score) => score,
            None => continue,
        };
        let score = number_score * 10 + address_score;
        if best.is_none_or(|(saved, _)| score > saved) {
            best = Some((score, entry));
        }
    }
    best.map(|(_, entry)| entry.data.clone())
}

/// How well one entry's address matches the display, with zero meaning not at all.
fn address_score(entry: &AuthorityEntry, parsed: &ParsedDisplay) -> Option<u16> {
    const FAMILY_INTERNET: u16 = 0;
    const FAMILY_LOCAL: u16 = 256;
    const FAMILY_WILD: u16 = 65535;
    match entry.family {
        FAMILY_WILD => Some(2),
        FAMILY_INTERNET => {
            let address = String::from_utf8_lossy(&entry.address);
            let host = match &parsed.target {
                DisplayTarget::Tcp { host, .. } => host.as_str(),
                DisplayTarget::Unix { .. } => "localhost",
            };
            if address == host
                || (is_loopback(&address) && is_loopback(host))
                || (address == "localhost" && parsed.local)
            {
                Some(3)
            } else {
                None
            }
        }
        // A local record names this machine, which is where a local display is.
        FAMILY_LOCAL if parsed.local => Some(2),
        _ => None,
    }
}

/// Whether an address names the loopback interface in one of its spellings.
fn is_loopback(address: &str) -> bool {
    address == "127.0.0.1" || address == "::1" || address == "localhost"
}

/// Parses the binary records of an X authority file.
///
/// A record whose length fields run past the end of the file is ignored with the rest
/// of the file: a truncated write must not produce a cookie that belongs to nothing.
pub(crate) fn parse_authority(bytes: &[u8]) -> Vec<AuthorityEntry> {
    let mut entries = Vec::new();
    let mut rest = bytes;
    while let Some((entry, used)) = read_entry(rest) {
        if used == 0 {
            break;
        }
        entries.push(entry);
        rest = &rest[used..];
    }
    entries
}

fn read_entry(bytes: &[u8]) -> Option<(AuthorityEntry, usize)> {
    let mut at = 0;
    let family = take_u16(bytes, &mut at)?;
    let address = take_field(bytes, &mut at)?;
    let number = String::from_utf8_lossy(&take_field(bytes, &mut at)?).into_owned();
    let name = String::from_utf8_lossy(&take_field(bytes, &mut at)?).into_owned();
    let data = take_field(bytes, &mut at)?;
    Some((
        AuthorityEntry {
            family,
            address,
            number,
            name,
            data,
        },
        at,
    ))
}

fn take_u16(bytes: &[u8], at: &mut usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let slice = bytes.get(*at..end)?;
    *at = end;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn take_field(bytes: &[u8], at: &mut usize) -> Option<Vec<u8>> {
    let length = usize::from(take_u16(bytes, at)?);
    let end = at.checked_add(length)?;
    let slice = bytes.get(*at..end)?.to_vec();
    *at = end;
    Some(slice)
}

// ---------------------------------------------------------------------------
// The restricted mode's credential
// ---------------------------------------------------------------------------

/// The names an `xauth` program has on this platform.
///
/// The program is looked up the way the system looks up any other: by these names in
/// the directories on `PATH`.
fn xauth_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["xauth.exe", "xauth.cmd", "xauth.bat", "xauth"]
    }
    #[cfg(not(windows))]
    {
        &["xauth"]
    }
}

/// The `xauth` program of this machine, if there is one.
fn xauth_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for name in xauth_names() {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Asks the local display for an untrusted cookie, the way `ssh -X` does.
fn issue_untrusted_cookie(display: &str, authority: &Path) -> Result<Vec<u8>, X11Failure> {
    issue_with(xauth_program().as_deref(), display, authority)
}

/// The same, from the program a caller names instead of the one on `PATH`.
///
/// `None` means this machine has no program that could issue the cookie, which is a
/// reason of its own: the restricted mode cannot be honoured by guessing.
///
/// The entry is written into a private file of this session, which is what OpenSSH's
/// client does for `ssh -X` (`client_x11_get_proto` makes a temporary directory, writes
/// the issued entry there, reads it back and removes it). The authority file of this
/// machine is named in `XAUTHORITY` so the program can reach the display, and is never
/// written to: `xauth generate` replaces the entry whose key matches the display, and
/// that entry is this machine's own credential. Both files carry the same key for a
/// local display, so writing in place would take the trusted cookie of the display away
/// from every other program that uses the same file.
fn issue_with(
    program: Option<&Path>,
    display: &str,
    authority: &Path,
) -> Result<Vec<u8>, X11Failure> {
    let Some(program) = program else {
        return Err(X11Failure::NoUntrustedCookie {
            detail: format!(
                "no {} program was found on PATH, and a restricted cookie has to be created with one",
                xauth_names().join("/")
            ),
        });
    };
    let issued = IssuedFile::create()?;
    run_generate(program, issued.path(), display, authority)?;
    let entries = parse_authority(&read_authority(issued.path())?);
    match the_one_cookie(&entries) {
        Issued::One(cookie) => Ok(cookie),
        Issued::None => Err(X11Failure::NoUntrustedCookie {
            detail: format!(
                "{} exited successfully but wrote no {AUTH_NAME} entry for {display}",
                program.display()
            ),
        }),
        // Two entries in a file this session alone writes means the display answered
        // twice or the program did something unexpected. Which of them belongs to this
        // session cannot be told, and guessing is not worth a credential.
        Issued::Many(count) => Err(X11Failure::NoUntrustedCookie {
            detail: format!(
                "{} wrote {count} {AUTH_NAME} entries for {display}, so which of them belongs to this session cannot be told",
                program.display()
            ),
        }),
    }
}

/// What the file the program was given holds.
enum Issued {
    /// Exactly one cookie: the one the display issued for this session.
    One(Vec<u8>),
    /// Nothing that could be presented.
    None,
    /// More than one cookie, which the session must refuse rather than guess between.
    Many(usize),
}

/// The one `MIT-MAGIC-COOKIE-1` cookie of the file the program wrote.
///
/// The file belongs to this session alone, so anything but one entry is an answer this
/// bridge cannot use: a session without a credential, or one that cannot tell which
/// entry is its own.
fn the_one_cookie(entries: &[AuthorityEntry]) -> Issued {
    let mut cookies = entries
        .iter()
        .filter(|entry| entry.name == AUTH_NAME)
        .map(|entry| entry.data.clone());
    match (cookies.next(), cookies.next()) {
        (None, _) => Issued::None,
        (Some(only), None) => Issued::One(only),
        (Some(_), Some(_)) => Issued::Many(2 + cookies.count()),
    }
}

/// The private file a restricted session's cookie is written to.
///
/// A directory of this session only, made as private as the platform allows
/// (`0700` where there are permission bits), holding one file. It is removed when the
/// session stops asking, whatever the answer was: what the display issued is a
/// credential, and a file left behind would be one more copy of it. Nothing of this
/// machine's own authority file is copied here.
struct IssuedFile {
    directory: PathBuf,
    file: PathBuf,
}

impl IssuedFile {
    /// Creates an empty private directory for one cookie.
    fn create() -> Result<Self, X11Failure> {
        let directory = std::env::temp_dir().join(format!("shellune-x11-{}", uuid::Uuid::new_v4()));
        #[allow(unused_mut)]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&directory)
            .map_err(|error| X11Failure::NoUntrustedCookie {
                detail: format!(
                    "a private directory for the restricted cookie could not be made in {}: {error}",
                    std::env::temp_dir().display()
                ),
            })?;
        let file = directory.join("xauthfile");
        Ok(Self { directory, file })
    }

    /// The file the program writes the entry to.
    fn path(&self) -> &Path {
        &self.file
    }
}

impl Drop for IssuedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

/// Reads one authority file, naming it when it cannot be read.
fn read_authority(authority: &Path) -> Result<Vec<u8>, X11Failure> {
    std::fs::read(authority).map_err(|error| X11Failure::NoUntrustedCookie {
        detail: format!("{}: {error}", authority.display()),
    })
}

/// Runs the one `xauth` command that registers an untrusted cookie.
///
/// The command is the one OpenSSH's own client uses for `ssh -X`: the display it talks
/// to, the protocol both ends speak, and `untrusted` as the trust level, which is what
/// makes the X server's SECURITY extension restrict what a client of that cookie may
/// do. A program that fails, or that does not answer, is a failure to report and never
/// a reason to forward with the trusted cookie instead.
///
/// `-f` is where the entry is written; `XAUTHORITY` stays on this machine's own
/// authority file, which is the one the program reads to authenticate to the display.
/// The two are deliberately different files: `xauth` writes - and on a local display
/// replaces - the entry named there, and that entry is not this session's to take.
fn run_generate(
    program: &Path,
    file: &Path,
    display: &str,
    authority: &Path,
) -> Result<(), X11Failure> {
    let mut child = Command::new(program)
        .arg("-f")
        .arg(file)
        .arg("generate")
        .arg(display)
        .arg(AUTH_NAME)
        .arg("untrusted")
        // The file the program has to reach the display with, named in the environment
        // as well: `-f` only says where the answer goes.
        .env("XAUTHORITY", authority)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| X11Failure::NoUntrustedCookie {
            detail: format!("{} could not be run: {error}", program.display()),
        })?;
    let deadline = Instant::now() + XAUTH_PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(X11Failure::NoUntrustedCookie {
                    detail: format!(
                        "{display} did not answer within {}s",
                        XAUTH_PATIENCE.as_secs()
                    ),
                });
            }
            Err(error) => {
                return Err(X11Failure::NoUntrustedCookie {
                    detail: format!("{} could not be waited for: {error}", program.display()),
                });
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| X11Failure::NoUntrustedCookie {
            detail: format!("{} left no result: {error}", program.display()),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Err(X11Failure::NoUntrustedCookie {
        detail: if said.is_empty() {
            format!(
                "{} could not create an untrusted cookie for {display} ({})",
                program.display(),
                output.status
            )
        } else {
            format!(
                "{} could not create an untrusted cookie for {display} ({}): {}",
                program.display(),
                output.status,
                said.chars().take(400).collect::<String>()
            )
        },
    })
}

// ---------------------------------------------------------------------------
// The X11 connection setup request
// ---------------------------------------------------------------------------

/// The length of a complete setup request, or `None` while it is still incomplete.
///
/// The setup request is the first thing an X11 client sends: a 12-byte header, the
/// authentication protocol name and the authentication data, each padded to four
/// bytes. Its length fields are written in the byte order the request declares.
fn setup_size(buffer: &[u8]) -> Option<usize> {
    if buffer.len() < 12 {
        return None;
    }
    let order = buffer[0];
    let name_len = usize::from(u16_at(buffer, 6, order)?);
    let data_len = usize::from(u16_at(buffer, 8, order)?);
    Some(12 + pad4(name_len) + pad4(data_len))
}

/// The connection setup request the local X server will see.
///
/// When the request carries an `MIT-MAGIC-COOKIE-1` cookie, that cookie is replaced by
/// the real one of this machine. The request is rebuilt rather than edited in place so
/// a cookie of a different length is written with the length and padding it needs.
/// Anything the client already sent behind the setup request is kept.
fn rewrite_setup(request: &[u8], credential: &[u8]) -> Vec<u8> {
    let Some(order) = request.first().copied() else {
        return Vec::new();
    };
    let (Some(name_len), Some(data_len)) = (
        u16_at(request, 6, order).map(usize::from),
        u16_at(request, 8, order).map(usize::from),
    ) else {
        return request.to_vec();
    };
    let name_start = 12;
    let Some(name) = request.get(name_start..name_start + name_len) else {
        return request.to_vec();
    };
    if name != AUTH_NAME.as_bytes() {
        // Some other protocol: this bridge has no translation for it, so the request
        // travels as it was written instead of being presented as something it is not.
        return request.to_vec();
    }
    let setup_len = 12 + pad4(name_len) + pad4(data_len);
    let mut out = Vec::with_capacity(setup_len + credential.len());
    out.push(order);
    out.push(0);
    out.extend_from_slice(&request[2..6]);
    push_u16(&mut out, name_len, order);
    push_u16(&mut out, credential.len(), order);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(name);
    out.extend(std::iter::repeat_n(0u8, pad4(name_len) - name_len));
    out.extend_from_slice(credential);
    out.extend(std::iter::repeat_n(
        0u8,
        pad4(credential.len()) - credential.len(),
    ));
    if let Some(rest) = request.get(setup_len..) {
        out.extend_from_slice(rest);
    }
    out
}

fn u16_at(bytes: &[u8], at: usize, order: u8) -> Option<u16> {
    let pair = [*bytes.get(at)?, *bytes.get(at + 1)?];
    Some(if order == b'l' {
        u16::from_le_bytes(pair)
    } else {
        u16::from_be_bytes(pair)
    })
}

fn push_u16(out: &mut Vec<u8>, value: usize, order: u8) {
    let value = u16::try_from(value).unwrap_or(u16::MAX);
    let bytes = if order == b'l' {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    out.extend_from_slice(&bytes);
}

fn pad4(length: usize) -> usize {
    (length + 3) & !3
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

// ---------------------------------------------------------------------------
// The local X server socket
// ---------------------------------------------------------------------------

/// A connection to the local X server, whichever way the display is reached.
enum DisplaySocket {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(TcpStream),
}

impl AsyncRead for DisplaySocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_read(cx, buffer),
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buffer),
        }
    }
}

impl AsyncWrite for DisplaySocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_write(cx, buffer),
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// Connects to the local X server of a display.
async fn open_display(target: &DisplayTarget) -> io::Result<DisplaySocket> {
    match target {
        #[cfg(unix)]
        DisplayTarget::Unix {
            path,
            abstract_name,
        } => {
            if let Ok(stream) = tokio::net::UnixStream::connect(path).await {
                return Ok(DisplaySocket::Unix(stream));
            }
            // A Linux X server may only have the abstract socket, which has no file in
            // the X11 directory; trying it is what makes such a server reachable.
            #[cfg(target_os = "linux")]
            {
                use std::os::linux::net::SocketAddrExt;
                if let Ok(address) =
                    std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
                    && let Ok(stream) = tokio::net::UnixStream::connect_addr(&address).await
                {
                    return Ok(DisplaySocket::Unix(stream));
                }
            }
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} cannot be opened", path.display()),
            ))
        }
        #[cfg(not(unix))]
        DisplayTarget::Unix { path, .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is a unix socket, which this platform cannot open",
                path.display()
            ),
        )),
        DisplayTarget::Tcp { host, port } => Ok(DisplaySocket::Tcp(
            TcpStream::connect((host.as_str(), *port)).await?,
        )),
    }
}

// ---------------------------------------------------------------------------
// The session's gate
// ---------------------------------------------------------------------------

/// What one shell channel asked the server for, in the order it was asked.
///
/// The server answers channel requests in order, so counting the answers is how the
/// X11 answer is told apart from the pseudo-terminal's and the shell's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Pty,
    X11,
    Shell,
}

/// The state the client handler and the session loop share.
struct GateInner {
    mode: X11Forwarding,
    /// Number of the shell channel whose answers are being counted, once it exists.
    shell: Option<u32>,
    /// Requests of that channel the server has not answered yet.
    pending: VecDeque<Request>,
    /// Where this session's channels are bridged, once it asked for forwarding.
    ready: Option<Ready>,
    tasks: Vec<JoinHandle<()>>,
    closed: bool,
}

/// The live part of a forwarding session: where to connect and with which cookie.
struct Ready {
    target: DisplayTarget,
    credential: Vec<u8>,
    display: String,
}

/// What one session's forwarding reports to the loop that owns its connection.
///
/// A refusal is not a status on its own: the reason the server refused is not in the
/// protocol, so the loop asks the server about its own `xauth` before deciding what the
/// user is told. Everything else is already decided here.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum X11Notice {
    /// A status to pass on as it is.
    Status(X11Status),
    /// The server answered the forwarding request with a failure.
    Refused,
}
/// One session's X11 forwarding, shared by its handler and its shell loop.
pub(crate) struct X11Gate {
    inner: Mutex<GateInner>,
    events: UnboundedSender<X11Notice>,
}

impl X11Gate {
    /// The gate of one attempt, plus the channel it reports its status on.
    pub(crate) fn channel(mode: X11Forwarding) -> (Arc<Self>, UnboundedReceiver<X11Notice>) {
        let (events, receiver) = unbounded_channel();
        (
            Arc::new(Self {
                inner: Mutex::new(GateInner {
                    mode,
                    shell: None,
                    pending: VecDeque::new(),
                    ready: None,
                    tasks: Vec::new(),
                    closed: false,
                }),
                events,
            }),
            receiver,
        )
    }

    /// A gate that accepts no X11 channel, for the hops in front of the target.
    ///
    /// A jump host has no forwarding request of this session, so a channel it opens
    /// for X11 never belongs to this machine's display.
    pub(crate) fn rejecting() -> Arc<Self> {
        let (gate, _receiver) = Self::channel(X11Forwarding::Off);
        gate
    }

    /// The mode this attempt was dialled with.
    pub(crate) fn mode(&self) -> X11Forwarding {
        self.lock()
            .map(|inner| inner.mode)
            .unwrap_or(X11Forwarding::Off)
    }

    /// Records the shell channel and the requests sent on it.
    ///
    /// Called once the channel is confirmed and before the answers can arrive, so an
    /// early `x11` answer is attributed to the request that produced it.
    pub(crate) fn shell_requests(&self, channel: u32, x11: bool) {
        if let Ok(mut inner) = self.lock() {
            inner.shell = Some(channel);
            inner.pending.clear();
            inner.pending.push_back(Request::Pty);
            if x11 {
                inner.pending.push_back(Request::X11);
            }
            inner.pending.push_back(Request::Shell);
        }
    }

    /// Installs what inbound channels are bridged to and reports "enabling".
    ///
    /// Done as soon as the request is sent: a server may open the first channel before
    /// it answers the request itself.
    pub(crate) fn announce(&self, prepared: &Prepared) {
        let mut reported = false;
        if let Ok(mut inner) = self.lock()
            && !inner.closed
        {
            inner.ready = Some(Ready {
                target: prepared.target.clone(),
                credential: prepared.credential.clone(),
                display: prepared.display.clone(),
            });
            reported = true;
        }
        if reported {
            let _ = self.events.send(X11Notice::Status(X11Status::Enabling));
        }
    }

    /// Applies one channel answer from the server.
    pub(crate) fn on_channel_reply(&self, channel: u32, accepted: bool) {
        let answer = {
            let Ok(mut inner) = self.lock() else {
                return;
            };
            if inner.shell != Some(channel) {
                return;
            }
            match inner.pending.pop_front() {
                Some(Request::X11) => Some(accepted),
                // An answer to the pseudo-terminal or the shell request says nothing
                // about X11, and a failed shell ends the session anyway.
                Some(Request::Pty | Request::Shell) => None,
                None => None,
            }
        };
        match answer {
            Some(true) => {
                let ready = self
                    .lock()
                    .ok()
                    .and_then(|inner| inner.ready.as_ref().map(|ready| ready.display.clone()));
                if let Some(display) = ready {
                    let _ = self
                        .events
                        .send(X11Notice::Status(X11Status::Ready { display }));
                }
            }
            Some(false) => {
                // Nothing may be bridged through a request the server refused, so the
                // local cookie stops being reachable from this session at once. Why it
                // refused is not in the protocol: the loop asks the server, which is
                // why this reports the refusal rather than a reason of its own.
                self.discard_ready();
                let _ = self.events.send(X11Notice::Refused);
            }
            None => {}
        }
    }

    /// Whether an inbound `x11` channel would be bridged right now.
    #[cfg(test)]
    pub(crate) fn accepts_inbound(&self) -> bool {
        self.lock()
            .map(|inner| !inner.closed && inner.ready.is_some())
            .unwrap_or(false)
    }

    /// Accepts one inbound `x11` channel and bridges it to the local display.
    ///
    /// A channel that arrives without an installed display is refused: accepting it
    /// would leave the remote program waiting for a bridge that cannot exist.
    pub(crate) async fn open_inbound(
        &self,
        channel: Channel<client::Msg>,
        reply: ChannelOpenHandle,
    ) {
        let resolved = {
            let Ok(inner) = self.lock() else {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
                return;
            };
            if inner.closed {
                None
            } else {
                inner.ready.as_ref().map(|ready| {
                    (
                        ready.target.clone(),
                        ready.credential.clone(),
                        ready.display.clone(),
                    )
                })
            }
        };
        let Some((target, cookie, display)) = resolved else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return;
        };
        reply.accept().await;
        let events = self.events.clone();
        let task = tokio::spawn(async move {
            if let Err(failure) = relay(channel.into_stream(), target, cookie).await {
                let _ = events.send(X11Notice::Status(X11Status::Failed {
                    reason: format!("X11 program from {display}: {failure}"),
                    next_step: failure.next_step(),
                }));
            }
        });
        if let Ok(mut inner) = self.lock() {
            if inner.closed {
                task.abort();
            } else {
                inner.tasks.push(task);
            }
        }
    }

    /// Stops accepting channels and drops the bridges that are open.
    ///
    /// Called when the session ends: after this returns, no new inbound channel can be
    /// accepted and no bridge keeps a socket to the local display.
    pub(crate) fn close(&self) {
        let tasks = {
            let Ok(mut inner) = self.lock() else {
                return;
            };
            inner.closed = true;
            inner.ready = None;
            inner.pending.clear();
            std::mem::take(&mut inner.tasks)
        };
        for task in tasks {
            task.abort();
        }
    }

    fn discard_ready(&self) {
        if let Ok(mut inner) = self.lock() {
            inner.ready = None;
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, GateInner>, ()> {
        self.inner.lock().map_err(|_| ())
    }
}

impl Drop for X11Gate {
    fn drop(&mut self) {
        self.close();
    }
}

/// What the server said about its own `xauth` (R11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RemoteXauth {
    /// The server has an `xauth` it could have run.
    Present,
    /// The server has none it could run, which is why it could not prepare a display.
    Missing {
        /// What the check found.
        detail: String,
    },
    /// The server would not say.
    Unknown {
        /// Why the answer is missing.
        detail: String,
    },
}

/// Asks the server whether it has an `xauth` it could have used.
///
/// The SSH protocol answers a refused `x11-req` with a failure and nothing else: a
/// server with `X11Forwarding no` and a server whose `xauth` cannot run look the same.
/// This asks the same authenticated connection one question, on a channel of its own,
/// so the refusal can name the cause it can prove. It is a best effort: a server that
/// cannot run the check leaves the refusal as it was.
pub(crate) async fn probe_remote_xauth<H: client::Handler>(
    handle: &client::Handle<H>,
) -> RemoteXauth {
    // The check is deliberately the same one sshd needs: a program called `xauth` that
    // this account can execute. It runs under `sh` so a login shell of another family
    // does not change what the answer means.
    const CHECK: &str = r#"sh -c 'p=$(command -v xauth 2>/dev/null) || exit 127; [ -n "$p" ] || exit 127; [ -x "$p" ] || exit 126'"#;
    let mut channel = match crate::session::open_exec_channel(handle, CHECK).await {
        Ok(channel) => channel,
        Err(
            crate::sftp::ChannelFailure::Unavailable { reason }
            | crate::sftp::ChannelFailure::Lost { reason },
        ) => {
            return RemoteXauth::Unknown { detail: reason };
        }
    };
    let deadline = tokio::time::Instant::now() + PROBE_PATIENCE;
    let mut exit: Option<u32> = None;
    let mut answered = tokio::time::Instant::now() + PROBE_PATIENCE;
    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                Some(ChannelMsg::Failure) => {
                    let _ = channel.close().await;
                    return RemoteXauth::Unknown {
                        detail: "the server refused to run the check".to_owned(),
                    };
                }
                // The status is sent before the channel closes, but the two can arrive
                // in either order; after the end of the output, only a short wait is
                // worth having.
                Some(ChannelMsg::Eof) => {
                    answered = answered.min(tokio::time::Instant::now() + Duration::from_millis(500));
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            () = tokio::time::sleep_until(deadline.min(answered)) => {
                let _ = channel.close().await;
                return RemoteXauth::Unknown {
                    detail: "the server did not answer the check in time".to_owned(),
                };
            }
        }
    }
    let _ = channel.close().await;
    match exit {
        Some(0) => RemoteXauth::Present,
        Some(126) => RemoteXauth::Missing {
            detail: "the xauth it finds is not executable".to_owned(),
        },
        Some(127) => RemoteXauth::Missing {
            detail: "no xauth was found".to_owned(),
        },
        Some(status) => RemoteXauth::Unknown {
            detail: format!("the check exited with {status}"),
        },
        None => RemoteXauth::Unknown {
            detail: "the check reported no status".to_owned(),
        },
    }
}

/// What the user is told about a refusal, after the server was asked why.
pub(crate) async fn refusal_status<H: client::Handler>(handle: &client::Handle<H>) -> X11Status {
    let failure = match probe_remote_xauth(handle).await {
        RemoteXauth::Missing { detail } => X11Failure::RemoteWithoutXauth { detail },
        RemoteXauth::Present => X11Failure::ForwardingForbidden,
        RemoteXauth::Unknown { detail } => X11Failure::Refused {
            reason: format!("the server did not say why it failed ({detail})"),
        },
    };
    X11Status::failed(&failure)
}

/// Copies one X11 channel to the local display, replacing the session cookie.
async fn relay<R>(remote: R, target: DisplayTarget, credential: Vec<u8>) -> Result<(), X11Failure>
where
    R: AsyncRead + AsyncWrite + Unpin,
{
    let mut local =
        open_display(&target)
            .await
            .map_err(|error| X11Failure::DisplayUnreachable {
                display: target.describe(),
                detail: error.to_string(),
            })?;
    let mut remote = remote;
    let setup = read_setup(&mut remote).await?;
    let rewritten = rewrite_setup(&setup, &credential);
    local
        .write_all(&rewritten)
        .await
        .map_err(|error| X11Failure::DisplayUnreachable {
            display: target.describe(),
            detail: error.to_string(),
        })?;
    local.flush().await.map_err(|error| X11Failure::Lost {
        detail: error.to_string(),
    })?;
    tokio::io::copy_bidirectional(&mut local, &mut remote)
        .await
        .map_err(|error| X11Failure::Lost {
            detail: error.to_string(),
        })?;
    Ok(())
}

/// Reads one X11 connection setup request from the channel.
async fn read_setup<R>(remote: &mut R) -> Result<Vec<u8>, X11Failure>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(size) = setup_size(&buffer)
            && buffer.len() >= size
        {
            return Ok(buffer);
        }
        let read = remote
            .read(&mut chunk)
            .await
            .map_err(|error| X11Failure::Lost {
                detail: error.to_string(),
            })?;
        if read == 0 {
            return Err(X11Failure::Lost {
                detail: "the remote program closed before it finished its X11 setup request"
                    .to_owned(),
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_SETUP {
            return Err(X11Failure::Lost {
                detail: "the remote program sent a setup request that is not X11".to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime")
            .block_on(future)
    }

    /// One authority record, in the binary form `xauth` writes.
    fn entry(family: u16, address: &str, number: &str, name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&family.to_be_bytes());
        for field in [address.as_bytes(), number.as_bytes(), name.as_bytes()] {
            out.extend_from_slice(&u16::try_from(field.len()).expect("short").to_be_bytes());
            out.extend_from_slice(field);
        }
        out.extend_from_slice(&u16::try_from(data.len()).expect("short").to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    /// A setup request as an Xlib client writes it.
    fn setup_request(name: &str, cookie: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut out = vec![b'l', 0];
        out.extend_from_slice(&11u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&u16::try_from(name.len()).expect("short").to_le_bytes());
        out.extend_from_slice(&u16::try_from(cookie.len()).expect("short").to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend(std::iter::repeat_n(0u8, pad4(name.len()) - name.len()));
        out.extend_from_slice(cookie);
        out.extend(std::iter::repeat_n(0u8, pad4(cookie.len()) - cookie.len()));
        out.extend_from_slice(extra);
        out
    }

    /// A prepared session as the gate tests need it, without a display or a file.
    fn prepared_for_gate() -> Prepared {
        Prepared {
            target: DisplayTarget::Tcp {
                host: "127.0.0.1".to_owned(),
                port: 6000,
            },
            screen: 0,
            display: ":0".to_owned(),
            authority: PathBuf::from("/tmp/shellune-test-Xauthority"),
            credential: vec![1u8; 16],
            remote_cookie: "00".repeat(16),
        }
    }

    /// A directory that holds one authority file, removed when the test ends.
    struct Authority {
        dir: PathBuf,
        path: PathBuf,
    }

    impl Authority {
        fn holding(cookie: &[u8]) -> Self {
            let dir = std::env::temp_dir().join(format!("shellune-x11-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("a test directory");
            let path = dir.join("Xauthority");
            std::fs::write(&path, entry(65535, "", "", AUTH_NAME, cookie))
                .expect("write the authority file");
            Self { dir, path }
        }
    }

    impl Drop for Authority {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The cookie the stand-in display issues for the restricted mode.
    const ISSUED: [u8; 16] = [0x5A; 16];

    /// A stand-in for the program that registers an untrusted cookie with the display.
    fn issue_issued_cookie(_display: &str, _authority: &Path) -> Result<Vec<u8>, X11Failure> {
        Ok(ISSUED.to_vec())
    }

    /// The same, for a display that cannot issue one.
    fn issue_nothing(_display: &str, _authority: &Path) -> Result<Vec<u8>, X11Failure> {
        Err(X11Failure::NoUntrustedCookie {
            detail: "the display has no SECURITY extension".to_owned(),
        })
    }

    #[test]
    fn a_display_name_becomes_the_address_and_screen_of_a_local_server() {
        let parsed = parse_display(":99").expect("a local display");
        assert_eq!(parsed.number, 99);
        assert_eq!(parsed.screen, 0);
        assert!(parsed.local);
        match parsed.target {
            #[cfg(unix)]
            DisplayTarget::Unix { path, .. } => {
                assert!(path.ends_with("X11-unix/X99"), "{}", path.display());
            }
            DisplayTarget::Tcp { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 6000 + 99);
            }
            // Only one of the two variants exists on the platform that builds here.
            #[allow(unreachable_patterns)]
            other => panic!("unexpected target: {other:?}"),
        }

        let screen = parse_display(":10.2").expect("a screen");
        assert_eq!(screen.number, 10);
        assert_eq!(screen.screen, 2);

        let tcp = parse_display("127.0.0.1:5").expect("a port display");
        assert!(matches!(
            tcp.target,
            DisplayTarget::Tcp { ref host, port } if host == "127.0.0.1" && port == 6005
        ));

        assert!(matches!(
            parse_display("").expect_err("an empty display"),
            X11Failure::NoDisplay { .. }
        ));
        assert!(matches!(
            parse_display("no-colon").expect_err("not a display name"),
            X11Failure::NoDisplay { .. }
        ));
        assert!(matches!(
            parse_display(":x").expect_err("not a number"),
            X11Failure::NoDisplay { .. }
        ));
    }

    #[test]
    fn the_cookie_belongs_to_the_display_and_not_only_to_the_file() {
        let wild = entry(65535, "", "", AUTH_NAME, &[1u8; 16]);
        let other_display = entry(65535, "", "7", AUTH_NAME, &[2u8; 16]);
        let this_display = entry(256, "host", "99", AUTH_NAME, &[3u8; 16]);
        let mut bytes = wild;
        bytes.extend_from_slice(&other_display);
        bytes.extend_from_slice(&this_display);
        let entries = parse_authority(&bytes);
        assert_eq!(entries.len(), 3);

        let parsed = parse_display(":99").expect("a display");
        let cookie = find_cookie(&entries, "99", &parsed).expect("a cookie for :99");
        assert_eq!(
            cookie,
            vec![3u8; 16],
            "the entry naming this display wins over a wildcard"
        );

        let parsed = parse_display(":7").expect("a display");
        assert_eq!(
            find_cookie(&entries, "7", &parsed),
            Some(vec![2u8; 16]),
            "a display with its own entry does not fall back to the wildcard"
        );

        let parsed = parse_display(":8").expect("a display");
        assert_eq!(
            find_cookie(&entries, "8", &parsed),
            Some(vec![1u8; 16]),
            "a display without an entry still has the wildcard"
        );
    }

    #[test]
    fn a_missing_authority_file_or_entry_names_what_is_missing() {
        let directory = std::env::temp_dir().join(format!("shellune-x11-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("a test directory");
        let missing = directory.join("Xauthority");
        let failure = prepare_from(":99", Some(&missing), None).expect_err("no authority file");
        assert!(
            matches!(failure, X11Failure::NoAuthority { .. }),
            "{failure:?}"
        );

        let empty = directory.join("empty-authority");
        std::fs::write(&empty, entry(256, "host", "7", AUTH_NAME, &[9u8; 16]))
            .expect("write the authority file");
        let failure = prepare_from(":99", Some(&empty), None).expect_err("no cookie for :99");
        assert!(
            matches!(&failure, X11Failure::NoAuthority { detail } if detail.contains(":99")),
            "{failure:?}"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_prepared_session_keeps_its_own_cookie_and_hands_out_another_one() {
        let directory = std::env::temp_dir().join(format!("shellune-x11-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("a test directory");
        let authority = directory.join("Xauthority");
        let real = [0xABu8; 16];
        std::fs::write(&authority, entry(65535, "", "", AUTH_NAME, &real))
            .expect("write the authority file");

        let prepared = prepare_from(":99", Some(&authority), None).expect("a prepared session");
        assert_eq!(
            prepared.request_cookie().len(),
            32,
            "a hex encoded 16-byte cookie"
        );
        assert_ne!(prepared.request_cookie().to_owned(), hex(&real));
        assert!(
            !prepared.request_cookie().contains("ababab"),
            "the real cookie must not be the one the server is told"
        );
        assert_eq!(prepared.display, ":99");
        assert_eq!(prepared.screen(), 0);

        let other = prepare_from(":99", Some(&authority), None).expect("another session");
        assert_ne!(
            prepared.request_cookie(),
            other.request_cookie(),
            "every session gets its own cookie"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_two_modes_forward_with_different_credentials() {
        let trusted_cookie = [0xABu8; 16];
        let authority = Authority::holding(&trusted_cookie);

        // The trusted mode presents this machine's own cookie to the display.
        let trusted =
            prepare_from("127.0.0.1:99", Some(&authority.path), None).expect("a prepared session");
        assert_eq!(
            trusted.credential,
            trusted_cookie.to_vec(),
            "the trusted mode forwards with the cookie of the authority file"
        );

        // The restricted mode replaces it with the untrusted one the display issued:
        // that cookie, and nothing else, is what makes the mode restricted.
        let mut restricted =
            prepare_from("127.0.0.1:99", Some(&authority.path), None).expect("a prepared session");
        assert_eq!(
            restricted.credential,
            trusted_cookie.to_vec(),
            "nothing is replaced before the display was asked"
        );
        block_on(restricted.restrict(issue_issued_cookie)).expect("an untrusted cookie");
        assert_eq!(restricted.credential, ISSUED.to_vec());
        assert_ne!(
            restricted.credential,
            trusted_cookie.to_vec(),
            "the restricted mode must not present the trusted cookie"
        );
        // What the server is told is neither of the two: it is this session's own.
        assert_eq!(restricted.request_cookie().len(), 32);
        assert_ne!(restricted.request_cookie().to_owned(), hex(&ISSUED));
    }

    #[test]
    fn a_display_that_cannot_issue_an_untrusted_cookie_stops_the_restricted_mode() {
        let authority = Authority::holding(&[0xABu8; 16]);
        let mut prepared =
            prepare_from("127.0.0.1:99", Some(&authority.path), None).expect("a prepared session");
        let failure = block_on(prepared.restrict(issue_nothing))
            .expect_err("a display without the extension cannot restrict anything");
        match &failure {
            X11Failure::NoUntrustedCookie { detail } => {
                assert!(detail.contains("SECURITY extension"), "{detail}");
            }
            other => panic!("the failure must name the missing cookie, got {other:?}"),
        }
        // The failure says what to do, and one of the ways out is the trusted mode,
        // which the user has to choose: the restricted mode never falls back to it.
        let next = failure.next_step();
        assert!(next.contains("xauth"), "{next}");
        assert!(next.to_lowercase().contains("trusted"), "{next}");
        assert!(
            !failure.to_string().contains("trusted cookie"),
            "nothing may claim the trusted cookie was used"
        );
    }

    #[test]
    fn an_untrusted_cookie_is_the_one_entry_of_this_sessions_own_file() {
        // What the display writes for `generate <display> untrusted`: one entry, keyed
        // the same way this machine's own entry for that display is keyed.
        let mut issued = entry(256, "testhost", "99", AUTH_NAME, &[2u8; 16]);
        let entries = parse_authority(&issued);
        assert!(
            matches!(&the_one_cookie(&entries), Issued::One(cookie) if cookie == &vec![2u8; 16]),
            "the one entry is the credential of the session"
        );

        // A program that exits without writing anything leaves nothing to present.
        assert!(matches!(the_one_cookie(&[]), Issued::None));
        // A file holding another protocol only is the same: this bridge speaks one.
        assert!(matches!(
            the_one_cookie(&parse_authority(&entry(
                256,
                "testhost",
                "99",
                "XDM-AUTHORIZATION-1",
                &[7u8; 24]
            ))),
            Issued::None
        ));

        // Two entries cannot be told apart, and the session refuses to guess which one
        // the display issued for it.
        issued.extend_from_slice(&entry(256, "otherhost", "99", AUTH_NAME, &[4u8; 16]));
        let entries = parse_authority(&issued);
        assert!(
            matches!(the_one_cookie(&entries), Issued::Many(2)),
            "an ambiguous answer is visible as two entries"
        );
    }

    #[test]
    fn the_issued_cookie_lives_in_a_private_file_that_is_removed_again() {
        let issued = IssuedFile::create().expect("a private directory");
        let path = issued.path().to_path_buf();
        assert!(
            path.starts_with(std::env::temp_dir()),
            "the file lives where temporary files live: {}",
            path.display()
        );
        assert!(!path.exists(), "nothing is in it before the program ran");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path.parent().expect("a directory"))
                .expect("the directory")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "only this user may read the cookie");
        }
        std::fs::write(&path, entry(256, "testhost", "99", AUTH_NAME, &[2u8; 16]))
            .expect("what the display issued");
        drop(issued);
        assert!(
            !path.exists(),
            "the cookie is removed with the file that held it"
        );
        assert!(
            !path.parent().expect("a directory").exists(),
            "the private directory goes with it"
        );
    }

    #[test]
    fn a_program_that_cannot_be_run_leaves_the_authority_file_alone() {
        let authority = Authority::holding(&[0xABu8; 16]);
        let missing = authority.dir.join("no-such-xauth");
        let failure = issue_with(Some(&missing), "127.0.0.1:99", &authority.path)
            .expect_err("a program that cannot be run issues no cookie");
        match &failure {
            X11Failure::NoUntrustedCookie { detail } => {
                assert!(detail.contains("no-such-xauth"), "{detail}");
            }
            other => panic!("the failure must name the program, got {other:?}"),
        }
        // The one file this machine's own programs authenticate with is untouched: the
        // restricted mode never writes to it, so a failed attempt cannot damage it.
        let bytes = std::fs::read(&authority.path).expect("the authority file");
        assert_eq!(bytes, entry(65535, "", "", AUTH_NAME, &[0xABu8; 16]));
    }

    #[test]
    fn a_machine_without_an_xauth_program_says_so_instead_of_forwarding_trusted() {
        let authority = Authority::holding(&[0xABu8; 16]);
        let failure = issue_with(None, "127.0.0.1:99", &authority.path)
            .expect_err("a cookie cannot be issued without a program");
        match &failure {
            X11Failure::NoUntrustedCookie { detail } => {
                assert!(detail.contains("xauth"), "{detail}");
                assert!(detail.contains("PATH"), "{detail}");
            }
            other => panic!("the failure must name the missing program, got {other:?}"),
        }
        let next = failure.next_step();
        assert!(next.contains("xauth"), "{next}");
        // The authority file is left exactly as it was: nothing was written to it.
        let bytes = std::fs::read(&authority.path).expect("the authority file");
        assert_eq!(bytes, entry(65535, "", "", AUTH_NAME, &[0xABu8; 16]));
    }

    #[test]
    fn a_quartz_display_name_is_a_socket_path_on_this_machine() {
        // XQuartz names its display by its launchd socket, not by `:0`.
        let parsed = parse_display("/private/tmp/com.apple.launchd.AbCdEf/org.xquartz:0")
            .expect("a mac display");
        assert_eq!(parsed.number, 0);
        assert_eq!(parsed.screen, 0);
        assert!(
            parsed.local,
            "the socket is on this machine, so its cookie is a local one"
        );
        match &parsed.target {
            DisplayTarget::Unix { path, .. } => assert_eq!(
                path,
                &PathBuf::from("/private/tmp/com.apple.launchd.AbCdEf/org.xquartz")
            ),
            other => panic!("a quartz display is a socket, not a machine: {other:?}"),
        }
    }

    #[test]
    fn a_setup_request_is_rewritten_with_the_real_cookie_and_keeps_what_follows() {
        let fake = [0x11u8; 16];
        let real = [0x22u8; 16];
        let request = setup_request(AUTH_NAME, &fake, b"next-packet");
        assert_eq!(
            setup_size(&request),
            Some(request.len() - "next-packet".len())
        );

        let rewritten = rewrite_setup(&request, &real);
        let rewritten_len = 12 + pad4(AUTH_NAME.len()) + pad4(real.len());
        assert_eq!(
            &rewritten[rewritten_len..],
            b"next-packet",
            "the bytes behind the setup request are kept"
        );
        assert!(
            !rewritten.windows(fake.len()).any(|window| window == fake),
            "the session cookie never reaches the local server"
        );
        assert!(
            rewritten.windows(real.len()).any(|window| window == real),
            "the local server sees this machine's cookie"
        );
        // The header the local server reads still describes the request it carries.
        assert_eq!(
            u16_at(&rewritten, 6, b'l'),
            Some(u16::try_from(AUTH_NAME.len()).expect("short"))
        );
        assert_eq!(
            u16_at(&rewritten, 8, b'l'),
            Some(u16::try_from(real.len()).expect("short"))
        );
        assert_eq!(setup_size(&rewritten), Some(rewritten_len));
    }

    #[test]
    fn a_setup_request_with_another_protocol_is_not_turned_into_a_cookie_request() {
        let request = setup_request("XDM-AUTHORIZATION-1", &[7u8; 24], b"");
        let rewritten = rewrite_setup(&request, &[0x22u8; 16]);
        assert_eq!(
            rewritten, request,
            "a protocol this bridge cannot translate travels unchanged"
        );
    }

    #[test]
    fn relay_replaces_the_session_cookie_before_the_local_server_sees_it() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a test listener");
        let port = listener.local_addr().expect("an address").port();
        let real = [0x33u8; 16];
        let fake = [0x44u8; 16];
        // The stand-in X server: it reads the setup request and answers with one byte,
        // which the client must receive through the bridge. The setup request of this
        // test is 48 bytes: a 12-byte header, a padded name and a padded cookie.
        let setup_len = 12 + pad4(AUTH_NAME.len()) + pad4(fake.len());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection");
            let mut buffer = vec![0u8; setup_len];
            std::io::Read::read_exact(&mut stream, &mut buffer).expect("a setup request");
            stream.write_all(&[0x01]).expect("an answer");
            stream.flush().expect("an answer");
            buffer
        });

        let (mut client, remote) = tokio::io::duplex(1024);
        let target = DisplayTarget::Tcp {
            host: "127.0.0.1".to_owned(),
            port,
        };
        let bridged = block_on(async {
            let bridge = tokio::spawn(relay(remote, target, real.to_vec()));
            client
                .write_all(&setup_request(AUTH_NAME, &fake, b""))
                .await
                .expect("a request");
            let mut answer = [0u8; 1];
            client.read_exact(&mut answer).await.expect("an answer");
            // Closing this end of the bridge is what ends it, as a program that exits
            // closes its display connection.
            drop(client);
            bridge.await.expect("the bridge finished")
        });
        assert!(bridged.is_ok(), "{bridged:?}");

        let seen = server.join().expect("the stand-in X server");
        assert!(
            seen.windows(real.len()).any(|window| window == real),
            "the local server is given this machine's cookie"
        );
        assert!(
            !seen.windows(fake.len()).any(|window| window == fake),
            "the session cookie is not what the local server sees"
        );
    }

    #[test]
    fn a_channel_arriving_before_the_display_is_known_is_refused() {
        let (gate, mut events) = X11Gate::channel(X11Forwarding::Restricted);
        assert_eq!(gate.mode(), X11Forwarding::Restricted);
        assert!(
            !gate.accepts_inbound(),
            "a request that was not accepted yet bridges nothing"
        );

        let prepared = prepared_for_gate();
        gate.announce(&prepared);
        assert!(gate.accepts_inbound());
        assert_eq!(
            events.try_recv(),
            Ok(X11Notice::Status(X11Status::Enabling))
        );

        // Closing the session stops accepting before anything else happens.
        gate.close();
        assert!(!gate.accepts_inbound());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn the_answer_to_the_x11_request_decides_the_session_status() {
        let (gate, mut events) = X11Gate::channel(X11Forwarding::Trusted);
        let channel = 7u32;
        let prepared = prepared_for_gate();
        gate.announce(&prepared);
        gate.shell_requests(channel, true);
        assert_eq!(
            events.try_recv(),
            Ok(X11Notice::Status(X11Status::Enabling))
        );

        // The pseudo-terminal was accepted first; that answer says nothing about X11.
        gate.on_channel_reply(channel, true);
        assert!(events.try_recv().is_err());

        gate.on_channel_reply(channel, true);
        assert_eq!(
            events.try_recv(),
            Ok(X11Notice::Status(X11Status::Ready {
                display: ":0".to_owned()
            }))
        );

        // The shell request's own answer is not a second status.
        gate.on_channel_reply(channel, true);
        assert!(events.try_recv().is_err());

        // A refused request drops the bridge at once and reports the refusal, so the
        // loop that owns the connection can ask the server why it happened.
        gate.shell_requests(channel, true);
        let _ = events.try_recv();
        gate.on_channel_reply(channel, true);
        gate.on_channel_reply(channel, false);
        assert_eq!(events.try_recv(), Ok(X11Notice::Refused));
        assert!(
            !gate.accepts_inbound(),
            "a refused request must not leave a bridge available"
        );
    }

    #[test]
    fn a_failure_says_what_to_do_about_it() {
        let failures = [
            X11Failure::NoDisplay {
                detail: "DISPLAY is not set".to_owned(),
            },
            X11Failure::NoAuthority {
                detail: "no cookie".to_owned(),
            },
            X11Failure::DisplayUnreachable {
                display: ":0".to_owned(),
                detail: "refused".to_owned(),
            },
            X11Failure::NoUntrustedCookie {
                detail: "no xauth on PATH".to_owned(),
            },
            X11Failure::RemoteWithoutXauth {
                detail: "no xauth was found".to_owned(),
            },
            X11Failure::ForwardingForbidden,
            X11Failure::Refused {
                reason: "X11 forwarding disabled".to_owned(),
            },
            X11Failure::Lost {
                detail: "closed".to_owned(),
            },
        ];
        let mut steps = Vec::new();
        let mut reasons = Vec::new();
        for failure in &failures {
            let next = failure.next_step();
            assert!(!next.is_empty(), "every failure has a next step: {failure}");
            assert!(!next.contains("hunter2"));
            steps.push(next);
            reasons.push(failure.to_string());
        }
        steps.sort();
        steps.dedup();
        assert_eq!(
            steps.len(),
            failures.len(),
            "the next steps stay distinguishable"
        );
        reasons.sort();
        reasons.dedup();
        assert_eq!(
            reasons.len(),
            failures.len(),
            "the reasons stay distinguishable"
        );
    }

    #[test]
    fn the_two_refusals_a_server_can_cause_are_not_one_message() {
        // A server without xauth cannot prepare a display; a server that has one and
        // still refuses has forwarding turned off or a broken XAuthLocation. The
        // protocol says neither, which is why the two are worded apart from each other
        // and from a refusal that could not be asked about at all.
        let missing = X11Failure::RemoteWithoutXauth {
            detail: "no xauth was found".to_owned(),
        };
        let forbidden = X11Failure::ForwardingForbidden;
        let unknown = X11Failure::Refused {
            reason: "the server did not say why it failed".to_owned(),
        };
        assert!(missing.to_string().contains("no usable xauth"));
        assert!(forbidden.to_string().contains("X11Forwarding"));
        assert!(!forbidden.to_string().contains("no usable xauth"));
        assert!(unknown.to_string().contains("did not say why"));
        assert!(missing.next_step().contains("Install xauth"));
        assert!(forbidden.next_step().contains("Turn on X11Forwarding"));
    }
}
