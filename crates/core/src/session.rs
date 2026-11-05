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
    /// A local shell that had started stopped accepting input or output.
    ///
    /// Kept apart from the launch failures because the process really did start:
    /// the reason is what the system said about the running session, and telling
    /// the user to check the program path would be the wrong advice.
    LocalShellIoFailed {
        /// The shell that was running.
        shell: String,
        /// What the system said, kept verbatim so it can be acted on.
        reason: String,
    },
    /// The remote address refused or could not be reached.
    ConnectionRefused,
    /// The connection attempt timed out.
    TimedOut,
    /// The transport could not be established or the handshake failed.
    ///
    /// Kept apart from `ConnectionRefused` and `TimedOut` because the reason comes from
    /// the network stack or the protocol layer, and "connection refused" would be the
    /// wrong advice for a protocol mismatch.
    ConnectionFailed {
        /// What the transport reported, already free of credentials.
        reason: String,
    },
    /// The server presented an unknown identity that was not accepted.
    HostKeyRejected,
    /// The server presented an identity that differs from the one that was trusted.
    ///
    /// This is the state that must never be resolved by connecting anyway: both
    /// fingerprints are carried so the shell can show what changed.
    HostIdentityChanged {
        /// Key algorithm the server presents now.
        algorithm: String,
        /// Fingerprint that was trusted earlier.
        stored: String,
        /// Fingerprint the server presents now.
        presented: String,
    },
    /// The system agent that was chosen is not reachable, or holds no usable key.
    AgentUnavailable {
        /// Which agent was tried.
        agent: crate::AgentKind,
    },
    /// The server rejected the offered credentials.
    AuthenticationFailed,
    /// The remote side closed the connection while the session was live.
    ConnectionLost,
    /// A live remote session stopped accepting input or output.
    ///
    /// The reason is what the transport reported about the running session, so the
    /// shell does not send the user to fix credentials that were already accepted.
    RemoteIoFailed {
        /// The host the session was talking to.
        host: String,
        /// What the transport reported.
        reason: String,
    },
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
            Self::LocalShellIoFailed { shell, reason } => {
                write!(
                    formatter,
                    "the local shell '{shell}' stopped responding: {reason}"
                )
            }
            Self::ConnectionFailed { reason } => {
                write!(
                    formatter,
                    "the connection could not be established: {reason}"
                )
            }
            Self::HostIdentityChanged {
                algorithm,
                stored,
                presented,
            } => write!(
                formatter,
                "the server identity changed: {algorithm} was {stored}, the server presents {presented}"
            ),
            Self::AgentUnavailable { agent } => {
                write!(formatter, "the {} is not available", agent.label())
            }
            Self::RemoteIoFailed { host, reason } => {
                write!(formatter, "the SSH session to {host} stopped: {reason}")
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

    /// Whether the session is on its way to being live.
    ///
    /// Used to keep a tab honest while an attempt is in flight: a session that is
    /// still connecting has neither a terminal nor a failure to report.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::VerifyingHost | Self::Authenticating
        )
    }

    /// Whether no further transition can happen without a new session.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Disconnected(_) | Self::Failed(_) | Self::Closed)
    }

    /// Whether a session in this state can start another process.
    ///
    /// A session that is live, connecting or closed cannot: the first two already
    /// own a process, and a closed session is final.
    #[must_use]
    pub fn can_reconnect(&self) -> bool {
        matches!(self, Self::Disconnected(_) | Self::Failed(_))
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

/// Which process an event belongs to.
///
/// A [SessionId] alone is not enough: a session that was reconnected keeps its tab
/// and its id, but the events of the process it used to run must not reach the new
/// one. The generation is incremented by every reconnect, so a late event from the
/// previous process carries an older generation and is dropped instead of being
/// drawn as output of the new shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTag {
    /// The session the event is for.
    pub session: SessionId,
    /// Which process of that session produced it.
    pub generation: u32,
}

impl EventTag {
    /// The tag of one process of one session.
    #[must_use]
    pub fn new(session: SessionId, generation: u32) -> Self {
        Self {
            session,
            generation,
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
    /// How many processes this session has run.
    ///
    /// Zero for the first one and incremented by every reconnect, which is what
    /// separates the events of a closed process from the events of the one that
    /// replaced it.
    #[serde(default)]
    generation: u32,
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
            generation: 0,
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

    /// Which process of this session is running, or last ran.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The tag every event of the current process carries.
    ///
    /// Identical for every event of one process, so a caller that hands events to a
    /// pane can drop the ones that belong to a process this session no longer runs.
    #[must_use]
    pub fn tag(&self) -> EventTag {
        EventTag::new(self.id, self.generation)
    }

    /// Whether an event tagged with `generation` still belongs to this session.
    ///
    /// A closed session accepts nothing: its process was ended on purpose, so an
    /// event that arrives afterwards describes something the user already dismissed.
    #[must_use]
    pub fn accepts_event(&self, generation: u32) -> bool {
        !matches!(self.state, SessionState::Closed) && generation == self.generation
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

    /// Starts another process for a session that ended.
    ///
    /// This is the manual reconnect: it is refused for a session that is still live,
    /// still connecting, or closed, so the caller cannot restart a shell that was
    /// never asked to restart. The generation moves on in the same step, which is
    /// what makes the previous process' late events unwelcome from here on.
    ///
    /// Nothing is replayed: the new process starts with an empty terminal, and the
    /// caller is responsible for not resending what was typed into the old one.
    ///
    /// # Errors
    ///
    /// Returns [SessionTransitionError::Illegal] when the session did not end, and
    /// [SessionTransitionError::AlreadyClosed] when it was closed.
    pub fn begin_reconnect(&mut self) -> Result<(), SessionTransitionError> {
        if matches!(self.state, SessionState::Closed) {
            return Err(SessionTransitionError::AlreadyClosed);
        }
        if !self.state.can_reconnect() {
            return Err(SessionTransitionError::Illegal {
                from: self.state.label(),
                to: SessionState::Connecting.label(),
            });
        }
        self.state = SessionState::Connecting;
        self.generation += 1;
        Ok(())
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
    fn a_changed_identity_states_both_fingerprints_and_stays_failed() {
        let mut session = remote_session();
        session.begin_connecting().expect("starts");
        session.begin_host_verification().expect("verifies");
        let failure = SessionFailure::HostIdentityChanged {
            algorithm: "ssh-ed25519".to_owned(),
            stored: "SHA256:old".to_owned(),
            presented: "SHA256:new".to_owned(),
        };
        session
            .fail(failure.clone())
            .expect("the change is recorded");

        assert_eq!(session.state(), &SessionState::Failed(failure));
        assert!(!session.state().accepts_input());

        let text = SessionFailure::HostIdentityChanged {
            algorithm: "ssh-ed25519".to_owned(),
            stored: "SHA256:old".to_owned(),
            presented: "SHA256:new".to_owned(),
        }
        .to_string();
        assert!(text.contains("SHA256:old"), "{text}");
        assert!(text.contains("SHA256:new"), "{text}");
        assert!(text.contains("ssh-ed25519"), "{text}");
    }

    #[test]
    fn transport_and_agent_failures_name_what_went_wrong() {
        assert_eq!(
            SessionFailure::ConnectionFailed {
                reason: "connection reset by peer".to_owned()
            }
            .to_string(),
            "the connection could not be established: connection reset by peer"
        );
        assert_eq!(
            SessionFailure::AgentUnavailable {
                agent: crate::AgentKind::OpenSshPipe
            }
            .to_string(),
            "the Windows OpenSSH agent is not available"
        );
        let io = SessionFailure::RemoteIoFailed {
            host: "deploy@10.0.0.7:22".to_owned(),
            reason: "the channel was closed".to_owned(),
        };
        assert_eq!(
            io.to_string(),
            "the SSH session to deploy@10.0.0.7:22 stopped: the channel was closed"
        );
        assert!(
            !io.to_string().contains("could not be established"),
            "a session that ran must not read as a connect failure"
        );
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
        // A session that started and then failed has to say so: the launch text
        // would tell the user to check a path that is fine.
        let io = SessionFailure::LocalShellIoFailed {
            shell: "pwsh".to_owned(),
            reason: "the pipe has been ended".to_owned(),
        };
        assert_eq!(
            io.to_string(),
            "the local shell 'pwsh' stopped responding: the pipe has been ended"
        );
        assert!(!io.to_string().contains("could not be started"));
    }
}
