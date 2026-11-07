//! What a connection attempt can fail at, said without a secret (R4, R6, R12).
//!
//! Every variant carries public material only: a host, a path, an algorithm, two
//! fingerprints, or the text the operating system and the transport produced. A
//! password or a passphrase is never formatted into one, which is what makes it safe
//! to show these in the shell, keep them in a session record and write them to a log.

use std::fmt;

use shellune_core::{AgentKind, HostId, SecurityError, SessionFailure};

use crate::hostkey::PresentedKey;

/// Why a remote session could not be opened, or stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// The address refused the connection.
    Refused {
        /// The address that was dialled, as the user typed it.
        target: String,
    },
    /// The address did not answer in time.
    TimedOut {
        /// The address that was dialled.
        target: String,
    },
    /// The transport or the protocol handshake failed for another reason.
    Unreachable {
        /// What the system or the transport reported.
        reason: String,
    },
    /// The user did not accept the unknown server identity.
    HostKeyRejected {
        /// The identity that was presented and refused.
        presented: PresentedKey,
    },
    /// The attempt ended while the user had not decided about an unknown identity.
    ///
    /// Kept apart from [ConnectError::TimedOut] because the server answered at once:
    /// the missing answer was the user's, so blaming the network would send them to
    /// look at a connection that was never the problem.
    HostKeyUndecided {
        /// The identity that was presented and never decided on.
        presented: PresentedKey,
    },
    /// The server presents a different identity than the one that was trusted.
    HostKeyChanged {
        /// Fingerprint that was trusted.
        stored: String,
        /// Identity the server presents now.
        presented: PresentedKey,
    },
    /// The identity could not be turned into something the user can compare.
    HostKeyUnsupported {
        /// What the transport reported about the key.
        reason: String,
    },
    /// The server rejected the credentials that were offered.
    AuthRejected,
    /// The private key file could not be read.
    KeyUnreadable {
        /// Path the user selected.
        path: String,
        /// What the operating system reported.
        reason: String,
    },
    /// The private key file is encrypted and needs a passphrase.
    KeyEncrypted {
        /// Path of the encrypted key.
        path: String,
    },
    /// The passphrase did not decrypt the key.
    KeyDecryptFailed {
        /// Path of the encrypted key.
        path: String,
    },
    /// The key file exists but is not a key this build understands.
    KeyInvalid {
        /// Path of the key.
        path: String,
        /// What the parser reported.
        reason: String,
    },
    /// The agent could not be reached.
    AgentUnavailable {
        /// Which agent was tried.
        agent: AgentKind,
        /// What the system reported.
        reason: String,
    },
    /// The agent is running but holds no identity.
    AgentHasNoIdentity {
        /// Which agent was tried.
        agent: AgentKind,
    },
    /// The agent refused to sign with the identity that was offered.
    AgentSignFailed {
        /// Which agent was tried.
        agent: AgentKind,
        /// What the agent reported.
        reason: String,
    },
    /// Every identity the agent holds was refused by the server.
    AgentIdentityRejected {
        /// Which agent was tried.
        agent: AgentKind,
    },
    /// The server refused the shell channel or its pseudo terminal.
    PtyRejected {
        /// What the transport reported.
        reason: String,
    },
    /// A live session stopped working.
    Disconnected {
        /// What the transport reported.
        reason: String,
    },
}

impl ConnectError {
    /// The connection could not be established at all, as opposed to being refused
    /// by the server after a successful handshake.
    #[must_use]
    pub fn is_transport_failure(&self) -> bool {
        matches!(
            self,
            Self::Refused { .. } | Self::TimedOut { .. } | Self::Unreachable { .. }
        )
    }

    /// Whether retrying with corrected input can reasonably succeed.
    ///
    /// A changed identity is the one case where retrying is the wrong advice: the
    /// server is not who it used to be, and repeating the attempt cannot fix that.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        // A missing decision is the one failure that connecting again fixes, because
        // the same question is asked and can be answered this time.
        !matches!(
            self,
            Self::HostKeyChanged { .. } | Self::HostKeyUnsupported { .. }
        )
    }

    /// What the user can do next, phrased for the failure view.
    #[must_use]
    pub fn next_step(&self) -> &'static str {
        match self {
            Self::Refused { .. } => "Check the address and port, then connect again.",
            Self::TimedOut { .. } => "Check the network and the address, then try again.",
            Self::Unreachable { .. } => "Check the address, then try again.",
            Self::HostKeyRejected { .. } => "The connection stops here; nothing was stored.",
            Self::HostKeyUndecided { .. } => {
                "Connect again and accept or refuse the identity while it is shown."
            }
            Self::HostKeyChanged { .. } => {
                "Do not continue. Check the server identity before trusting it again."
            }
            Self::HostKeyUnsupported { .. } => {
                "The server key cannot be compared; connect with a client that supports it."
            }
            Self::AuthRejected => "Check the credentials, or change the authentication method.",
            Self::KeyUnreadable { .. } => "Choose a key file this account can read.",
            Self::KeyEncrypted { .. } => "Enter the key passphrase to continue.",
            Self::KeyDecryptFailed { .. } => "Enter the correct passphrase for this key.",
            Self::KeyInvalid { .. } => "Choose a key file Shellune can read.",
            Self::AgentUnavailable { .. } => "Start the system SSH agent, then connect again.",
            Self::AgentHasNoIdentity { .. } => "Load a key into the agent, then connect again.",
            Self::AgentSignFailed { .. } => "Use another identity, or a key file instead.",
            Self::AgentIdentityRejected { .. } => "Load an identity the server accepts.",
            Self::PtyRejected { .. } => "The server refused a terminal; use another account.",
            Self::Disconnected { .. } => "Connect again to open a new session.",
        }
    }

    /// Builds the reason for a transport error the SSH layer reported.
    ///
    /// A refused connection and a timeout are recognisable from the operating
    /// system's error, and naming them separately is the difference between advice
    /// that helps and advice that does not.
    #[must_use]
    pub fn from_transport(target: &str, error: &russh::Error) -> Self {
        let text = error.to_string();
        let lower = text.to_lowercase();
        if lower.contains("refused") {
            return Self::Refused {
                target: target.to_owned(),
            };
        }
        if lower.contains("timed out") || lower.contains("timeout") {
            return Self::TimedOut {
                target: target.to_owned(),
            };
        }
        Self::Unreachable { reason: text }
    }

    /// The session failure that records this problem.
    #[must_use]
    pub fn session_failure(&self) -> SessionFailure {
        match self {
            Self::Refused { .. } => SessionFailure::ConnectionRefused,
            Self::TimedOut { .. } => SessionFailure::TimedOut,
            Self::Unreachable { reason } => SessionFailure::ConnectionFailed {
                reason: reason.clone(),
            },
            Self::HostKeyRejected { .. } => SessionFailure::HostKeyRejected,
            // Not a rejection and not a network problem: the session records that it
            // could not be established, and the reason says which decision is missing.
            Self::HostKeyUndecided { .. } => SessionFailure::ConnectionFailed {
                reason: self.to_string(),
            },
            Self::HostKeyChanged { stored, presented } => SessionFailure::HostIdentityChanged {
                algorithm: presented.algorithm.clone(),
                stored: stored.clone(),
                presented: presented.fingerprint.clone(),
            },
            Self::HostKeyUnsupported { reason } => SessionFailure::ConnectionFailed {
                reason: reason.clone(),
            },
            Self::AuthRejected => SessionFailure::AuthenticationFailed,
            // The credentials could not be used at all, so the connection never
            // reached the server: saying "the server rejected them" would be wrong.
            Self::KeyUnreadable { reason, .. }
            | Self::KeyInvalid { reason, .. }
            | Self::AgentSignFailed { reason, .. }
            | Self::PtyRejected { reason } => SessionFailure::ConnectionFailed {
                reason: reason.clone(),
            },
            Self::KeyEncrypted { .. } | Self::KeyDecryptFailed { .. } => {
                SessionFailure::ConnectionFailed {
                    reason: self.to_string(),
                }
            }
            Self::AgentUnavailable { agent, .. } => {
                SessionFailure::AgentUnavailable { agent: *agent }
            }
            Self::AgentHasNoIdentity { agent } | Self::AgentIdentityRejected { agent } => {
                SessionFailure::AgentUnavailable { agent: *agent }
            }
            Self::Disconnected { reason } => SessionFailure::RemoteIoFailed {
                host: "the server".to_owned(),
                reason: reason.clone(),
            },
        }
    }

    /// The security view of this failure, when it is one the user must decide about.
    #[must_use]
    pub fn security_error(&self) -> Option<SecurityError> {
        match self {
            Self::HostKeyRejected { .. } => Some(SecurityError::HostKeyRejected),
            Self::HostKeyChanged { stored, presented } => Some(SecurityError::HostKeyMismatch {
                algorithm: presented.algorithm.clone(),
                stored: stored.clone(),
                presented: presented.fingerprint.clone(),
            }),
            Self::AuthRejected => Some(SecurityError::CredentialsRejected),
            Self::AgentUnavailable { agent, .. } => {
                Some(SecurityError::AgentUnavailable { agent: *agent })
            }
            Self::AgentHasNoIdentity { agent } => {
                Some(SecurityError::AgentHasNoIdentity { agent: *agent })
            }
            Self::AgentSignFailed { agent, .. } => {
                Some(SecurityError::AgentSignatureFailed { agent: *agent })
            }
            Self::AgentIdentityRejected { .. } => Some(SecurityError::ServerRejectedIdentity),
            Self::KeyEncrypted { path } | Self::KeyDecryptFailed { path } => {
                Some(SecurityError::PassphraseRequired { path: path.clone() })
            }
            Self::KeyUnreadable { path, .. } | Self::KeyInvalid { path, .. } => {
                Some(SecurityError::IdentityUnreadable { path: path.clone() })
            }
            Self::Refused { .. }
            | Self::TimedOut { .. }
            | Self::Unreachable { .. }
            | Self::HostKeyUndecided { .. }
            | Self::HostKeyUnsupported { .. }
            | Self::PtyRejected { .. }
            | Self::Disconnected { .. } => None,
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused { target } => write!(formatter, "{target} refused the connection"),
            Self::TimedOut { target } => write!(formatter, "{target} did not answer in time"),
            Self::Unreachable { reason } => {
                write!(
                    formatter,
                    "the connection could not be established: {reason}"
                )
            }
            Self::HostKeyRejected { presented } => write!(
                formatter,
                "the {} identity {} was not accepted",
                presented.algorithm, presented.fingerprint
            ),
            Self::HostKeyUndecided { presented } => write!(
                formatter,
                "the {} identity {} was neither accepted nor refused before the attempt ended",
                presented.algorithm, presented.fingerprint
            ),
            Self::HostKeyChanged { stored, presented } => write!(
                formatter,
                "the server identity changed: {} was trusted, the server now presents {}",
                stored, presented.fingerprint
            ),
            Self::HostKeyUnsupported { reason } => {
                write!(formatter, "the server key cannot be compared: {reason}")
            }
            Self::AuthRejected => {
                formatter.write_str("the server rejected the offered credentials")
            }
            Self::KeyUnreadable { path, reason } => {
                write!(
                    formatter,
                    "the private key at '{path}' could not be read: {reason}"
                )
            }
            Self::KeyEncrypted { path } => {
                write!(formatter, "the private key at '{path}' needs a passphrase")
            }
            Self::KeyDecryptFailed { path } => {
                write!(
                    formatter,
                    "the passphrase did not decrypt the key at '{path}'"
                )
            }
            Self::KeyInvalid { path, reason } => {
                write!(
                    formatter,
                    "the private key at '{path}' is not usable: {reason}"
                )
            }
            Self::AgentUnavailable { agent, reason } => {
                write!(
                    formatter,
                    "the {} is not available: {reason}",
                    agent.label()
                )
            }
            Self::AgentHasNoIdentity { agent } => {
                write!(formatter, "the {} holds no usable key", agent.label())
            }
            Self::AgentSignFailed { agent, reason } => {
                write!(
                    formatter,
                    "the {} refused to sign with the selected key: {reason}",
                    agent.label()
                )
            }
            Self::AgentIdentityRejected { agent } => write!(
                formatter,
                "the server accepted none of the keys held by the {}",
                agent.label()
            ),
            Self::PtyRejected { reason } => {
                write!(formatter, "the server refused a remote terminal: {reason}")
            }
            Self::Disconnected { reason } => write!(formatter, "the session stopped: {reason}"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Which hop of a jump chain an event or a failure belongs to.
///
/// The id is the saved host, and the address and port are the ones the trust
/// record must use. A later hop never borrows the target's identity: the prompt
/// and the failure name this hop even when the user asked to reach something
/// beyond it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HopLocation {
    /// Saved host this hop is.
    pub host_id: HostId,
    /// Zero-based position in the resolved chain. The target is last.
    pub index: usize,
    /// Display name of the host.
    pub name: String,
    /// Address that was dialled, not the display name.
    pub address: String,
    /// TCP port that was dialled.
    pub port: u16,
    /// Login user for this hop.
    pub user: String,
}

impl HopLocation {
    /// `user@address:port`, the form the single-hop errors already use.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}@{}:{}", self.user, self.address, self.port)
    }

    /// One-based hop number, for a message a person reads.
    #[must_use]
    pub fn number(&self) -> usize {
        self.index + 1
    }
}

impl fmt::Display for HopLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "hop {} \"{}\" ({})",
            self.number(),
            self.name,
            self.target()
        )
    }
}

/// Why a jump chain stopped.
///
/// A refusal that happens before dialling has no hop socket and no child process.
/// A hop failure names that hop and carries the same credential-free error a direct
/// connection would. Cancelling and refusing a ProxyCommand are their own cases so
/// they cannot be read as a server that rejected a password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainFailure {
    /// The chain was refused before any socket or process existed.
    NotDialed {
        /// The host the attempt was aimed at.
        host_id: HostId,
        /// What to change. Never includes a secret.
        message: String,
    },
    /// One hop failed. Later hops are not left running.
    ///
    /// The hop and the transport error are boxed so a `Result` can carry this
    /// failure without moving a large enum through every dial step.
    Hop {
        /// The hop that failed.
        hop: Box<HopLocation>,
        /// What failed, without credentials.
        error: Box<ConnectError>,
    },
    /// The attempt was cancelled. Child processes are stopped.
    Cancelled {
        /// The hop that was in progress, when one was.
        hop: Option<Box<HopLocation>>,
    },
    /// A ProxyCommand was shown and not confirmed, so it was not started.
    ProxyNotConfirmed {
        /// The hop whose command was refused.
        hop: Box<HopLocation>,
        /// The expanded command that was not run.
        command: String,
    },
}

impl ChainFailure {
    /// The hop a person should look at, when the failure has one.
    #[must_use]
    pub fn hop(&self) -> Option<&HopLocation> {
        match self {
            Self::Hop { hop, .. } | Self::ProxyNotConfirmed { hop, .. } => Some(hop.as_ref()),
            Self::Cancelled { hop } => hop.as_deref(),
            Self::NotDialed { .. } => None,
        }
    }

    /// The transport error, when this failure is one hop's connection error.
    #[must_use]
    pub fn connect_error(&self) -> Option<&ConnectError> {
        match self {
            Self::Hop { error, .. } => Some(error.as_ref()),
            Self::NotDialed { .. } | Self::Cancelled { .. } | Self::ProxyNotConfirmed { .. } => {
                None
            }
        }
    }

    /// The session failure that records this problem.
    #[must_use]
    pub fn session_failure(&self) -> SessionFailure {
        match self {
            Self::Hop { error, .. } => error.session_failure(),
            Self::NotDialed { message, .. } => SessionFailure::ConnectionFailed {
                reason: message.clone(),
            },
            Self::Cancelled { .. } | Self::ProxyNotConfirmed { .. } => {
                SessionFailure::ConnectionFailed {
                    reason: self.to_string(),
                }
            }
        }
    }
}

impl fmt::Display for ChainFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDialed { message, .. } => formatter.write_str(message),
            Self::Hop { hop, error } => write!(formatter, "{hop} failed: {error}"),
            Self::Cancelled { hop: Some(hop) } => write!(
                formatter,
                "the attempt was cancelled while connecting to {hop}"
            ),
            Self::Cancelled { hop: None } => {
                formatter.write_str("the attempt was cancelled before a connection was opened")
            }
            Self::ProxyNotConfirmed { hop, command } => write!(
                formatter,
                "the ProxyCommand for {hop} was not confirmed, so it was not run: {command}"
            ),
        }
    }
}

impl std::error::Error for ChainFailure {}

/// The transport error type the SSH client needs for its handler.
///
/// The library requires From<russh::Error>, and every transport error that reaches
/// this point is either a failure to establish the connection or a live connection
/// that stopped, which the mapping below keeps apart.
impl From<russh::Error> for ConnectError {
    fn from(error: russh::Error) -> Self {
        Self::Unreachable {
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presented() -> PresentedKey {
        PresentedKey {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: shellune_core::sha256_fingerprint(&[3u8; 32]).expect("a digest"),
            public_key: "AAAAC3NzaC1lZDI1NTE5".to_owned(),
        }
    }

    #[test]
    fn a_changed_identity_is_not_retryable_and_points_at_the_change() {
        let error = ConnectError::HostKeyChanged {
            stored: "SHA256:stored".to_owned(),
            presented: presented(),
        };
        assert!(!error.is_retryable());
        assert!(error.next_step().starts_with("Do not continue."));
        let text = error.to_string();
        assert!(text.contains("SHA256:stored"), "{text}");
        assert!(text.contains(&presented().fingerprint), "{text}");

        match error.session_failure() {
            SessionFailure::HostIdentityChanged {
                stored,
                presented: presented_now,
                ..
            } => {
                assert_eq!(stored, "SHA256:stored");
                assert_eq!(presented_now, presented().fingerprint);
            }
            other => panic!("a changed identity must stay a change, got {other:?}"),
        }
        assert!(matches!(
            error.security_error(),
            Some(SecurityError::HostKeyMismatch { .. })
        ));
    }

    #[test]
    fn a_rejected_identity_never_reads_as_a_connection_problem() {
        let error = ConnectError::HostKeyRejected {
            presented: presented(),
        };
        assert!(error.is_retryable(), "another host can still be tried");
        assert_eq!(
            error.session_failure(),
            SessionFailure::HostKeyRejected,
            "the session records a refusal, not a network failure"
        );
        assert!(error.to_string().contains("was not accepted"));
    }

    #[test]
    fn transport_failures_are_named_from_the_systems_own_words() {
        let refused = ConnectError::from_transport(
            "deploy@10.0.0.7:22",
            &russh::Error::IO(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
        );
        assert!(matches!(refused, ConnectError::Refused { .. }));
        assert_eq!(refused.session_failure(), SessionFailure::ConnectionRefused);

        let timeout = ConnectError::from_transport(
            "10.0.0.7:22",
            &russh::Error::IO(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        );
        assert!(matches!(timeout, ConnectError::TimedOut { .. }));
        assert_eq!(timeout.session_failure(), SessionFailure::TimedOut);
    }

    #[test]
    fn a_credential_problem_is_not_reported_as_a_server_rejection() {
        let error = ConnectError::KeyEncrypted {
            path: "/home/deploy/.ssh/id_ed25519".to_owned(),
        };
        assert!(
            !error.to_string().contains("server rejected"),
            "the server was never asked: {error}"
        );
        assert!(matches!(
            error.security_error(),
            Some(SecurityError::PassphraseRequired { .. })
        ));
        assert!(error.next_step().contains("passphrase"));
    }

    #[test]
    fn a_chain_failure_names_its_hop_and_not_a_secret() {
        let hop = HopLocation {
            host_id: shellune_core::HostId::new(),
            index: 1,
            name: "web".to_owned(),
            address: "10.0.0.8".to_owned(),
            port: 22,
            user: "deploy".to_owned(),
        };
        let failure = ChainFailure::Hop {
            hop: Box::new(hop.clone()),
            error: Box::new(ConnectError::AuthRejected),
        };
        let text = format!("{failure} {failure:?}");
        assert!(text.contains("hop 2"));
        assert!(text.contains("web"));
        assert!(text.contains("deploy@10.0.0.8:22"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        assert_eq!(
            failure.session_failure(),
            SessionFailure::AuthenticationFailed
        );

        let refused = ChainFailure::ProxyNotConfirmed {
            hop: Box::new(hop),
            command: "nc 10.0.0.8 22".to_owned(),
        };
        assert!(refused.to_string().contains("was not run"));
        assert!(refused.connect_error().is_none());
    }

    #[test]
    fn no_error_text_carries_a_secret() {
        let errors = [
            ConnectError::AuthRejected,
            ConnectError::AgentHasNoIdentity {
                agent: AgentKind::UnixSocket,
            },
            ConnectError::KeyDecryptFailed {
                path: "/keys/work".to_owned(),
            },
            ConnectError::Disconnected {
                reason: "the channel was closed".to_owned(),
            },
        ];
        for error in errors {
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.is_empty());
            assert!(!rendered.contains("hunter2"));
        }
    }
}
