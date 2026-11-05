//! Session identity, kind and state machine (R2, R6, R7).
//!
//! A session only reports [`SessionState::Connected`] after the transition that
//! justifies it, so the UI layer cannot show a live terminal for a connection that
//! failed, was rejected or was never authenticated.

use serde::{Deserialize, Serialize};

use crate::ids::{HostId, SessionId};

/// What a session runs against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    /// A shell process on this machine.
    Local {
        /// Shell executable, or `None` for the platform default.
        shell: Option<String>,
    },
    /// An SSH session to a saved host.
    Remote {
        /// The host the session was started from.
        host: HostId,
    },
}

impl SessionKind {
    /// Short label used in tab titles and confirmations.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local { .. } => "Local",
            Self::Remote { .. } => "SSH",
        }
    }

    /// Whether the session needs host identity verification and authentication.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

/// Why a session stopped being live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionFailure {
    /// The local shell could not be started.
    LocalShellUnavailable {
        /// The shell that was requested.
        shell: String,
    },
    /// The local shell could not be started for a reason the system reported.
    LocalShellLaunchFailed {
        /// The shell that was requested.
        shell: String,
        /// What the system said, kept verbatim so it can be acted on.
        reason: String,
    },
    /// The local shell exists but the system refused to run it.
    ///
    /// Kept apart from `LocalShellUnavailable` because the two need different
    /// fixes: one is a wrong path, the other is a permission problem that
    /// retyping the same path cannot solve.
    LocalShellPermissionDenied {
        /// The shell that was requested.
        shell: String,
    },
    /// The remote address refused or could not be reached.
    ConnectionRefused,
    /// The connection attempt timed out.
    TimedOut,
    /// The server presented an unknown or changed identity that was not accepted.
    HostKeyRejected,
    /// The server rejected the offered credentials.
    AuthenticationFailed,
    /// The remote side closed the connection while the session was live.
    ConnectionLost,
    /// The local shell process exited.
    ProcessExited {
        /// Exit code, when the platform reported one.
        code: Option<i32>,
    },
    /// The local shell was killed by a signal instead of exiting.
    ProcessSignalled {
        /// Name of the signal the system reported.
        signal: String,
    },
}

impl std::fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalShellUnavailable { shell } => {
                write!(formatter, "the local shell '{shell}' could not be started")
            }
            Self::LocalShellPermissionDenied { shell } => {
                write!(
                    formatter,
                    "the local shell '{shell}' could not be started because access was denied"
                )
            }
            Self::ConnectionRefused => formatter.write_str("the connection was refused"),
            Self::TimedOut => formatter.write_str("the connection timed out"),
            Self::HostKeyRejected => formatter.write_str("the server identity was not accepted"),
            Self::AuthenticationFailed => {
                formatter.write_str("the server rejected the credentials")
            }
            Self::ConnectionLost => formatter.write_str("the connection was lost"),
            Self::ProcessExited { code } => match code {
                Some(code) => write!(formatter, "the shell exited with code {code}"),
                None => formatter.write_str("the shell exited"),
            },
            Self::ProcessSignalled { signal } => {
                write!(formatter, "the shell was terminated by {signal}")
            }
            Self::LocalShellLaunchFailed { shell, reason } => {
                write!(
                    formatter,
                    "the local shell '{shell}' could not be started: {reason}"
                )
            }
        }
    }
}

/// Lifecycle state of one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionState {
    /// Created, not started.
    #[default]
    Idle,
    /// Waiting for the local process or the remote transport.
    Connecting,
    /// Remote only: the host identity is being verified with the user.
    VerifyingHost,
    /// Waiting for credentials to be accepted.
    Authenticating,
    /// Terminal input and output are live.
    Connected,
    /// Was live, then the peer or process went away.
    Disconnected(SessionFailure),
    /// Never became live: startup failed.
    Failed(SessionFailure),
    /// Closed by the user.
    Closed,
}

impl SessionState {
    /// Whether terminal input may be sent for this state.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// Whether the session reached the live state at least once.
    #[must_use]
    pub fn was_live(&self) -> bool {
        matches!(self, Self::Connected | Self::Disconnected(_))
    }

    /// Whether no further transition can happen without a new session.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Disconnected(_) | Self::Failed(_) | Self::Closed)
    }

    /// Short label for tabs and status text.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "Not started",
            Self::Connecting => "Connecting",
            Self::VerifyingHost => "Verifying host",
            Self::Authenticating => "Authenticating",
            Self::Connected => "Connected",
            Self::Disconnected(_) => "Disconnected",
            Self::Failed(_) => "Failed",
            Self::Closed => "Closed",
        }
    }
}

/// Why a state change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionTransitionError {
    /// The requested transition is not part of the lifecycle.
    #[error("a session cannot move from {from} to {to}")]
    Illegal {
        /// Current state label.
        from: &'static str,
        /// Requested state label.
        to: &'static str,
    },
    /// The session already ended and cannot be reused.
    #[error("the session is already closed")]
    AlreadyClosed,
}

/// One terminal session, local or remote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    id: SessionId,
    kind: SessionKind,
    title: String,
    state: SessionState,
}

impl Session {
    /// Creates a session that has not been started yet.
    #[must_use]
    pub fn new(kind: SessionKind, title: impl Into<String>) -> Self {
        let title = title.into();
        let fallback = match &kind {
            SessionKind::Local { shell } => {
                shell.clone().unwrap_or_else(|| "Local shell".to_owned())
            }
            SessionKind::Remote { .. } => "SSH session".to_owned(),
        };
        let title = if title.trim().is_empty() {
            fallback
        } else {
            title.trim().to_owned()
        };
        Self {
            id: SessionId::new(),
            kind,
            title,
            state: SessionState::Idle,
        }
    }

    /// Identifier used to route output and lifecycle events.
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// What the session runs against.
    #[must_use]
    pub fn kind(&self) -> &SessionKind {
        &self.kind
    }

    /// Title shown on the tab.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Current lifecycle state.
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    fn advance(
        &mut self,
        next: SessionState,
        allowed_from: &[SessionState],
    ) -> Result<(), SessionTransitionError> {
        if matches!(self.state, SessionState::Closed) {
            return Err(SessionTransitionError::AlreadyClosed);
        }
        if !allowed_from.contains(&self.state) {
            return Err(SessionTransitionError::Illegal {
                from: self.state.label(),
                to: next.label(),
            });
        }
        self.state = next;
        Ok(())
    }

    /// Starts the session: spawns the local shell or opens the remote transport.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] when the session is not idle or is closed.
    pub fn begin_connecting(&mut self) -> Result<(), SessionTransitionError> {
        self.advance(SessionState::Connecting, &[SessionState::Idle])
    }

    /// Moves a remote session to host identity verification.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] unless the session is connecting.
    pub fn begin_host_verification(&mut self) -> Result<(), SessionTransitionError> {
        if let SessionKind::Local { .. } = self.kind {
            return Err(SessionTransitionError::Illegal {
                from: self.state.label(),
                to: SessionState::VerifyingHost.label(),
            });
        }
        self.advance(SessionState::VerifyingHost, &[SessionState::Connecting])
    }

    /// Records that the user accepted the server identity and authentication began.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] unless the session is verifying the host.
    pub fn begin_authentication(&mut self) -> Result<(), SessionTransitionError> {
        self.advance(SessionState::Authenticating, &[SessionState::VerifyingHost])
    }

    /// Marks the terminal as live.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] unless the session finished connecting, or
    /// for a remote session unless the host was verified and the user authenticated.
    pub fn connected(&mut self) -> Result<(), SessionTransitionError> {
        let allowed_from: &[SessionState] = match self.kind {
            SessionKind::Local { .. } => &[SessionState::Connecting],
            SessionKind::Remote { .. } => &[SessionState::Authenticating],
        };
        self.advance(SessionState::Connected, allowed_from)
    }

    /// Records that a live session lost its peer or process.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] unless the session is live.
    pub fn disconnected(&mut self, failure: SessionFailure) -> Result<(), SessionTransitionError> {
        self.advance(
            SessionState::Disconnected(failure),
            &[SessionState::Connected],
        )
    }

    /// Records a start-up or connect failure.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError`] when the session is already live or ended;
    /// a live session must use [`Session::disconnected`] instead.
    pub fn fail(&mut self, failure: SessionFailure) -> Result<(), SessionTransitionError> {
        self.advance(
            SessionState::Failed(failure),
            &[
                SessionState::Idle,
                SessionState::Connecting,
                SessionState::VerifyingHost,
                SessionState::Authenticating,
            ],
        )
    }

    /// Closes the session from any state that has not ended yet.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTransitionError::AlreadyClosed`] when it is already closed.
    pub fn close(&mut self) -> Result<(), SessionTransitionError> {
        if matches!(self.state, SessionState::Closed) {
            return Err(SessionTransitionError::AlreadyClosed);
        }
        self.state = SessionState::Closed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_session() -> Session {
        Session::new(
            SessionKind::Remote {
                host: HostId::new(),
            },
            "prod web",
        )
    }

    #[test]
    fn a_remote_session_cannot_be_connected_before_verifying_the_host() {
        let mut session = remote_session();
        session
            .begin_connecting()
            .expect("idle session starts connecting");

        assert_eq!(
            session.connected(),
            Err(SessionTransitionError::Illegal {
                from: "Connecting",
                to: "Connected",
            })
        );
        assert_eq!(session.state(), &SessionState::Connecting);
        assert!(!session.state().accepts_input());
    }

    #[test]
    fn a_rejected_host_identity_never_becomes_a_live_session() {
        let mut session = remote_session();
        session.begin_connecting().expect("starts connecting");
        session.begin_host_verification().expect("verifies host");

        session
            .fail(SessionFailure::HostKeyRejected)
            .expect("rejection is recorded");
        assert_eq!(
            session.state(),
            &SessionState::Failed(SessionFailure::HostKeyRejected)
        );
        assert!(!session.state().accepts_input());
        assert!(!session.state().was_live());
        assert!(session.state().is_terminal());
    }

    #[test]
    fn the_full_remote_path_ends_live() {
        let mut session = remote_session();
        session.begin_connecting().expect("starts connecting");
        session.begin_host_verification().expect("verifies host");
        session.begin_authentication().expect("authenticates");
        session.connected().expect("connects");

        assert_eq!(session.state(), &SessionState::Connected);
        assert!(session.state().accepts_input());

        session
            .disconnected(SessionFailure::ConnectionLost)
            .expect("live session disconnects");
        assert_eq!(
            session.state(),
            &SessionState::Disconnected(SessionFailure::ConnectionLost)
        );
        assert!(!session.state().accepts_input());
        assert!(session.state().was_live());
    }

    #[test]
    fn a_local_session_connects_without_host_verification() {
        let mut session = Session::new(
            SessionKind::Local {
                shell: Some("powershell.exe".to_owned()),
            },
            "",
        );
        assert_eq!(session.title(), "powershell.exe");

        assert_eq!(
            session.begin_host_verification(),
            Err(SessionTransitionError::Illegal {
                from: "Not started",
                to: "Verifying host",
            })
        );

        session.begin_connecting().expect("starts");
        session
            .connected()
            .expect("local session connects directly");
        assert!(session.state().accepts_input());
    }

    #[test]
    fn a_live_session_records_a_disconnect_instead_of_a_start_failure() {
        let mut session = remote_session();
        session.begin_connecting().expect("starts");
        session.begin_host_verification().expect("verifies");
        session.begin_authentication().expect("authenticates");
        session.connected().expect("connects");

        assert_eq!(
            session.fail(SessionFailure::ConnectionRefused),
            Err(SessionTransitionError::Illegal {
                from: "Connected",
                to: "Failed",
            })
        );
    }

    #[test]
    fn closing_is_final_and_idempotence_is_reported() {
        let mut session = remote_session();
        session.close().expect("idle session closes");
        assert_eq!(session.state(), &SessionState::Closed);
        assert_eq!(session.close(), Err(SessionTransitionError::AlreadyClosed));
        assert_eq!(
            session.begin_connecting(),
            Err(SessionTransitionError::AlreadyClosed)
        );
    }

    #[test]
    fn every_state_has_a_distinct_label() {
        let states = [
            SessionState::Idle,
            SessionState::Connecting,
            SessionState::VerifyingHost,
            SessionState::Authenticating,
            SessionState::Connected,
            SessionState::Disconnected(SessionFailure::ConnectionLost),
            SessionState::Failed(SessionFailure::TimedOut),
            SessionState::Closed,
        ];
        let mut labels: Vec<&str> = states.iter().map(SessionState::label).collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count);
    }

    #[test]
    fn failures_explain_themselves_without_secrets() {
        assert_eq!(
            SessionFailure::LocalShellUnavailable {
                shell: "/bin/nosuch".to_owned()
            }
            .to_string(),
            "the local shell '/bin/nosuch' could not be started"
        );
        assert_eq!(
            SessionFailure::ProcessExited { code: Some(3) }.to_string(),
            "the shell exited with code 3"
        );
        assert_eq!(
            SessionFailure::AuthenticationFailed.to_string(),
            "the server rejected the credentials"
        );
    }
}
