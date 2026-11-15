//! Starting the disposable OpenSSH server without turning one hiccup into a skip.
//!
//! Every SSH integration test starts a real `sshd` inside the machine's Linux
//! environment. Two measured facts about that make a single attempt a bad bet:
//!
//! * the port is chosen by asking the operating system for a free one and is then
//!   handed to `sshd` in a separate process, so two test binaries that are handed the
//!   same number in that gap cannot both listen on it; and
//! * `cargo test --workspace` runs those binaries at the same time on one machine, so
//!   that gap is open constantly, and the WSL instance they all share answers slower
//!   the more of them ask at once.
//!
//! One lost race is therefore not evidence about this host at all, and the files that
//! mapped it to `None` printed `SKIPPED` and exited 0 - a skip standing in for a pass,
//! with nine cases claiming nothing while looking green.
//!
//! So a start is attempted [ATTEMPTS] times, each attempt asking for its own free port,
//! and only when every one of them failed is the host reported as unable to run a
//! server. That report carries the reason of each attempt, so a skip says what really
//! happened instead of guessing. The retries are bounded on purpose: a genuinely absent
//! `sshd` must not turn one test into minutes of waiting.

#![allow(dead_code)]

use std::fmt;
use std::time::Duration;

use shellune_ssh::testserver::{Harness, X11Server};

/// How many times a server is started before this host is reported as unable to run one.
pub const ATTEMPTS: u32 = 3;

/// How long the first pause between two attempts lasts; it doubles after that.
///
/// A lost port race is over as soon as the winner is listening, which is milliseconds;
/// the pause exists so that a slower WSL instance has handed the port back before the
/// next attempt asks for one.
pub const FIRST_PAUSE: Duration = Duration::from_millis(750);

/// Why this host could not run a disposable server, and what every attempt said.
#[derive(Debug, Clone)]
pub struct Unavailable {
    what: String,
    reasons: Vec<String>,
}

impl Unavailable {
    /// A host that cannot run this fixture at all, before any attempt was made.
    ///
    /// [Harness::is_available] answering `false` is such a fact: the Linux environment
    /// or its `sshd` is missing, which no retry can change.
    pub fn absent(what: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            reasons: vec![why.into()],
        }
    }

    /// What each failed attempt reported, in the order they were made.
    #[must_use]
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }
}

impl fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "this host could not start {} in {} attempts",
            self.what,
            self.reasons.len()
        )?;
        for (number, reason) in self.reasons.iter().enumerate() {
            write!(formatter, "; attempt {}: {reason}", number + 1)?;
        }
        Ok(())
    }
}

/// How long to pause after that many consecutive failures.
///
/// Doubling, and bounded by [ATTEMPTS]: the first pause is [FIRST_PAUSE], the second twice
/// that, so a caller retrying something other than a server start uses this schedule
/// instead of inventing a second one.
#[must_use]
pub fn backoff(failures: u32) -> Duration {
    FIRST_PAUSE * 2u32.saturating_pow(failures.saturating_sub(1).min(4))
}

/// Runs `attempt` until it succeeds or [ATTEMPTS] attempts have failed.
///
/// Each attempt is a whole start, so a caller asks for a fresh port inside the closure
/// instead of closing over one. The last failure is not treated differently from the
/// first: what decides a skip is that all of them failed, not which one was last.
pub fn start<T>(
    what: &str,
    mut attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, Unavailable> {
    let mut reasons = Vec::new();
    for number in 1..=ATTEMPTS {
        match attempt() {
            Ok(started) => return Ok(started),
            Err(reason) => reasons.push(reason),
        }
        if number < ATTEMPTS {
            std::thread::sleep(backoff(number));
        }
    }
    Err(Unavailable {
        what: what.to_owned(),
        reasons,
    })
}

/// Starts the ordinary disposable server under `name`, retrying a lost start.
///
/// # Errors
///
/// Returns [Unavailable] with every attempt's reason when no attempt started a server.
pub fn server(name: &str) -> Result<Harness, Unavailable> {
    let what = format!("the disposable server \"{name}\"");
    if !Harness::is_available() {
        return Err(Unavailable::absent(
            what,
            "this host has no Linux environment to run a disposable sshd in",
        ));
    }
    start(&what, || Harness::start(name, Harness::free_port()))
}

/// The same server for a test that cannot run at all without one.
///
/// The panic names every attempt, so a failure on a machine that *can* run a server
/// says what went wrong instead of only that something did.
#[must_use]
pub fn server_or_panic(name: &str) -> Harness {
    server(name).unwrap_or_else(|reason| panic!("{reason}"))
}

/// The disposable server without an SFTP subsystem, retrying a lost start.
///
/// # Errors
///
/// Returns [Unavailable] with every attempt's reason when no attempt started a server.
pub fn server_without_sftp(name: &str) -> Result<Harness, Unavailable> {
    let what = format!("the disposable server \"{name}\" without SFTP");
    if !Harness::is_available() {
        return Err(Unavailable::absent(
            what,
            "this host has no Linux environment to run a disposable sshd in",
        ));
    }
    start(&what, || {
        Harness::start_without_sftp(name, Harness::free_port())
    })
}

/// The disposable server that refuses TCP forwarding, retrying a lost start.
///
/// # Errors
///
/// Returns [Unavailable] with every attempt's reason when no attempt started a server.
pub fn server_without_forwarding(name: &str) -> Result<Harness, Unavailable> {
    let what = format!("the disposable server \"{name}\" without forwarding");
    if !Harness::is_available() {
        return Err(Unavailable::absent(
            what,
            "this host has no Linux environment to run a disposable sshd in",
        ));
    }
    start(&what, || {
        Harness::start_without_forwarding(name, Harness::free_port())
    })
}

/// The disposable server with a chosen X11 behaviour, retrying a lost start.
///
/// # Errors
///
/// Returns [Unavailable] with every attempt's reason when no attempt started a server.
pub fn server_with_x11(name: &str, x11: X11Server) -> Result<Harness, Unavailable> {
    let what = format!("the disposable server \"{name}\" with {x11:?}");
    if !Harness::is_available() {
        return Err(Unavailable::absent(
            what,
            "this host has no Linux environment to run a disposable sshd in",
        ));
    }
    start(&what, || {
        Harness::start_with_x11(name, Harness::free_port(), x11)
    })
}

/// The same, for a test that cannot run at all without a server.
#[must_use]
pub fn server_with_x11_or_panic(name: &str, x11: X11Server) -> Harness {
    server_with_x11(name, x11).unwrap_or_else(|reason| panic!("{reason}"))
}
