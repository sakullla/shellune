//! Why a serial port could not be opened, configured or kept running (R17, R21).
//!
//! R21 asks for a specific reason for a missing port, a wrong setting, a port that
//! is already open and a missing permission, and R17 asks for a final state instead
//! of an endless "connecting". This module is where that honesty lives:
//!
//! * every error names one [`SerialCause`] the user can act on, and keeps the causes
//!   the system reported but could not separate in [`SerialError::causes`] - a Windows
//!   `ERROR_ACCESS_DENIED` when opening a COM port means "in use by another program"
//!   *or* "you may not open it", and pretending it means only one of the two would
//!   send the user to the wrong place;
//! * the raw operating system code and its own message are kept verbatim in
//!   [`RawError`], so a diagnostic can be copied out without this crate paraphrasing
//!   what the driver said;
//! * nothing here formats a byte of the traffic a port carried: an error talks about
//!   a port name, a setting, an error code and the system's own text.

use std::fmt;

/// Why a serial port could not be opened, configured or kept running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SerialCause {
    /// No port of that name exists on this machine.
    NotFound,
    /// Another program (or this process) already holds the port.
    InUse,
    /// The port exists but this process may not open it.
    PermissionDenied,
    /// The driver rejected the settings, or the settings cannot exist.
    UnsupportedConfig,
    /// The device went away mid-session: it was unplugged, or the driver dropped it.
    Disconnected,
    /// The port reported an input/output failure that is none of the above.
    Io,
    /// The list of ports could not be read.
    Enumerate,
    /// The send queue is at its limit; the bytes were refused, not dropped.
    QueueFull,
    /// The session is no longer open, so the request cannot be carried out.
    Closed,
    /// The request was refused before it reached the port.
    Refused,
}

impl SerialCause {
    /// A short label for this cause, for a status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotFound => "port not found",
            Self::InUse => "port is in use",
            Self::PermissionDenied => "permission denied",
            Self::UnsupportedConfig => "settings not supported by the driver",
            Self::Disconnected => "device disconnected",
            Self::Io => "input/output failure",
            Self::Enumerate => "could not list ports",
            Self::QueueFull => "send queue is full",
            Self::Closed => "session is closed",
            Self::Refused => "request refused",
        }
    }

    /// The next step this cause points at, so a view never has to invent one.
    #[must_use]
    pub fn next_step(self) -> &'static str {
        match self {
            Self::NotFound => "Refresh the port list and pick an attached device.",
            Self::InUse => "Close the other program that holds this port, then open it again.",
            Self::PermissionDenied => {
                "Fix the permission for this device (a group membership on Unix, a device policy on Windows)."
            }
            Self::UnsupportedConfig => {
                "Choose settings the device accepts and open the port again."
            }
            Self::Disconnected => "Reconnect the device, refresh the list and reopen the port.",
            Self::Io => "Check the cable and the driver, then open the port again.",
            Self::Enumerate => "Check that the device service is running, then refresh the list.",
            Self::QueueFull => "Wait for the queued bytes to leave before sending more.",
            Self::Closed => "Open the port again before sending.",
            Self::Refused => "Send at least one byte.",
        }
    }
}

/// Which numbering a raw code belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawCodeKind {
    /// A Windows error code, as the system returned it.
    Win32,
    /// A Unix `errno` value.
    Errno,
}

impl RawCodeKind {
    /// How to name the numbering in a diagnostic.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Win32 => "Win32 error",
            Self::Errno => "errno",
        }
    }
}

/// The operating system's own code and message, kept so a diagnostic can be exact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawError {
    kind: RawCodeKind,
    code: i32,
    text: String,
}

impl RawError {
    /// Keeps a system code together with the message the system prints for it.
    #[must_use]
    pub fn new(kind: RawCodeKind, code: i32) -> Self {
        let text = std::io::Error::from_raw_os_error(code).to_string();
        Self { kind, code, text }
    }

    /// Which numbering the code belongs to.
    #[must_use]
    pub fn kind(&self) -> RawCodeKind {
        self.kind
    }

    /// The code itself, unchanged.
    #[must_use]
    pub fn code(&self) -> i32 {
        self.code
    }

    /// The message the system prints for that code.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for RawError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}: {}",
            self.kind.label(),
            self.code,
            self.text.trim_end()
        )
    }
}

/// A serial failure: one actionable cause, any causes that could not be separated,
/// and the system's own code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialError {
    primary: SerialCause,
    also: Vec<SerialCause>,
    port: Option<String>,
    detail: Option<String>,
    raw: Option<RawError>,
}

impl SerialError {
    /// Starts an error with the cause the system allowed to be named.
    #[must_use]
    pub fn new(primary: SerialCause) -> Self {
        Self {
            primary,
            also: Vec::new(),
            port: None,
            detail: None,
            raw: None,
        }
    }

    /// Names the port this happened to.
    #[must_use]
    pub fn at(mut self, port: impl Into<String>) -> Self {
        self.port = Some(port.into());
        self
    }

    /// Adds a cause the system also allows, without promoting it to the primary one.
    #[must_use]
    pub fn also(mut self, cause: SerialCause) -> Self {
        if cause != self.primary && !self.also.contains(&cause) {
            self.also.push(cause);
        }
        self
    }

    /// Adds fixed text that explains this particular refusal.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Keeps the code and message the system reported.
    #[must_use]
    pub fn with_raw(mut self, raw: RawError) -> Self {
        self.raw = Some(raw);
        self
    }

    /// The cause to show first.
    #[must_use]
    pub fn cause(&self) -> SerialCause {
        self.primary
    }

    /// The other causes the system allowed, when the system could not separate them.
    #[must_use]
    pub fn causes(&self) -> &[SerialCause] {
        &self.also
    }

    /// The port this happened to, when the failure is about one.
    #[must_use]
    pub fn port(&self) -> Option<&str> {
        self.port.as_deref()
    }

    /// Fixed explanatory text, when this refusal has one.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// The system code and message, when the failure came from the system.
    #[must_use]
    pub fn raw(&self) -> Option<&RawError> {
        self.raw.as_ref()
    }

    /// Whether this failure is, or may be, the given cause.
    #[must_use]
    pub fn is(&self, cause: SerialCause) -> bool {
        self.primary == cause || self.also.contains(&cause)
    }

    /// The send queue refused the bytes because it was at its limit.
    #[must_use]
    pub fn queue_full(capacity_frames: usize, capacity_bytes: usize) -> Self {
        Self::new(SerialCause::QueueFull).with_detail(format!(
            "the queue holds at most {capacity_frames} unsent frames or {capacity_bytes} bytes"
        ))
    }

    /// The session is already gone.
    #[must_use]
    pub fn closed() -> Self {
        Self::new(SerialCause::Closed)
    }
}

impl fmt::Display for SerialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(port) = &self.port {
            write!(formatter, "{port}: ")?;
        }
        formatter.write_str(self.primary.label())?;
        if !self.also.is_empty() {
            formatter.write_str(", or ")?;
            for (index, cause) in self.also.iter().enumerate() {
                if index > 0 {
                    formatter.write_str(" / ")?;
                }
                formatter.write_str(cause.label())?;
            }
        }
        if let Some(detail) = &self.detail {
            write!(formatter, " ({detail})")?;
        }
        if let Some(raw) = &self.raw {
            write!(formatter, " [{raw}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for SerialError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_combined_cause_keeps_every_reason_and_the_raw_code() {
        let error = SerialError::new(SerialCause::PermissionDenied)
            .at("COM3")
            .also(SerialCause::InUse)
            .with_raw(RawError::new(RawCodeKind::Win32, 5));

        assert_eq!(error.cause(), SerialCause::PermissionDenied);
        assert_eq!(error.causes(), [SerialCause::InUse]);
        assert!(error.is(SerialCause::InUse));
        assert!(!error.is(SerialCause::NotFound));
        assert_eq!(error.raw().map(RawError::code), Some(5));
        let text = error.to_string();
        assert!(text.starts_with("COM3: permission denied"), "{text}");
        assert!(text.contains("in use"), "{text}");
        assert!(text.contains("Win32 error 5"), "{text}");
    }

    #[test]
    fn a_repeated_cause_is_not_listed_twice() {
        let error = SerialError::new(SerialCause::InUse)
            .also(SerialCause::InUse)
            .also(SerialCause::Io)
            .also(SerialCause::Io);
        assert_eq!(error.causes(), [SerialCause::Io]);
    }

    #[test]
    fn every_cause_offers_a_next_step() {
        for cause in [
            SerialCause::NotFound,
            SerialCause::InUse,
            SerialCause::PermissionDenied,
            SerialCause::UnsupportedConfig,
            SerialCause::Disconnected,
            SerialCause::Io,
            SerialCause::Enumerate,
            SerialCause::QueueFull,
            SerialCause::Closed,
            SerialCause::Refused,
        ] {
            assert!(!cause.label().is_empty());
            assert!(!cause.next_step().is_empty());
        }
    }
}
