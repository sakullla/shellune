//! Secret handling and safe, actionable security errors (R4, R6, R12).
//!
//! Passwords and key passphrases live in [`SecretString`], which has no `Display`
//! and redacts `Debug`, and every [`SecurityError`] variant carries fixed text and
//! public material only. An authentication failure therefore cannot echo the value
//! the user typed, in the UI, in a log or in a panic message.

use std::fmt;

use zeroize::Zeroize;

/// A password or passphrase that never appears in formatted output.
pub struct SecretString {
    value: String,
}

impl SecretString {
    /// Wraps a secret. The value is zeroed when the wrapper is dropped.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// Borrows the secret for the single place that needs to authenticate with it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// Whether the secret is empty, which is worth refusing before a connection attempt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self::new(self.value.clone())
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString(<redacted>)")
    }
}

/// Which system SSH agent was used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    /// `SSH_AUTH_SOCK` on Unix.
    UnixSocket,
    /// The Windows OpenSSH named pipe.
    OpenSshPipe,
    /// Pageant on Windows.
    Pageant,
}

impl AgentKind {
    /// Label used in error text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::UnixSocket => "SSH_AUTH_SOCK agent",
            Self::OpenSshPipe => "Windows OpenSSH agent",
            Self::Pageant => "Pageant",
        }
    }
}

/// A security relevant failure with a message that is safe to show and log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecurityError {
    /// The agent could not be reached.
    AgentUnavailable {
        /// Which agent was tried.
        agent: AgentKind,
    },
    /// The agent is running but holds no identity.
    AgentHasNoIdentity {
        /// Which agent was tried.
        agent: AgentKind,
    },
    /// The agent refused to sign with the selected identity.
    AgentSignatureFailed {
        /// Which agent was tried.
        agent: AgentKind,
    },
    /// The private key file could not be read.
    IdentityUnreadable {
        /// Path the user selected.
        path: String,
    },
    /// The private key is encrypted and needs a passphrase.
    PassphraseRequired {
        /// Path of the encrypted key.
        path: String,
    },
    /// The stored trust record does not match the key the server presented.
    HostKeyMismatch {
        /// Key algorithm reported by the server.
        algorithm: String,
        /// Fingerprint that was accepted earlier.
        stored: String,
        /// Fingerprint the server presents now.
        presented: String,
    },
    /// The server identity is not known yet.
    UnknownHostKey {
        /// Key algorithm reported by the server.
        algorithm: String,
        /// Fingerprint of the presented key.
        fingerprint: String,
    },
    /// The user refused the presented server identity.
    HostKeyRejected,
    /// The passphrase or password was refused.
    CredentialsRejected,
    /// The server refused the offered key or agent identity.
    ServerRejectedIdentity,
}

impl SecurityError {
    /// Whether retrying with corrected input can reasonably succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::HostKeyRejected | Self::HostKeyMismatch { .. })
    }

    /// What the user can do next, phrased for the error view.
    #[must_use]
    pub fn next_step(&self) -> &'static str {
        match self {
            Self::AgentUnavailable { .. } => "Start the system SSH agent, then connect again.",
            Self::AgentHasNoIdentity { .. } => "Load a key into the agent, then connect again.",
            Self::AgentSignatureFailed { .. } => "Select another identity or use a key file.",
            Self::IdentityUnreadable { .. } => "Choose a readable private key file.",
            Self::PassphraseRequired { .. } => "Enter the key passphrase to continue.",
            Self::HostKeyMismatch { .. } => {
                "Do not continue. Check the server identity before trusting it again."
            }
            Self::UnknownHostKey { .. } => "Compare the fingerprint, then accept or reject it.",
            Self::HostKeyRejected => "The connection stops here; choose another host if needed.",
            Self::CredentialsRejected => "Check the password or passphrase, or change the method.",
            Self::ServerRejectedIdentity => "Use another authentication method or identity.",
        }
    }
}

impl fmt::Display for SecurityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentUnavailable { agent } => {
                write!(formatter, "the {} is not available", agent.label())
            }
            Self::AgentHasNoIdentity { agent } => {
                write!(formatter, "the {} holds no usable key", agent.label())
            }
            Self::AgentSignatureFailed { agent } => {
                write!(
                    formatter,
                    "the {} refused to sign with the selected key",
                    agent.label()
                )
            }
            Self::IdentityUnreadable { path } => {
                write!(formatter, "the private key at '{path}' could not be read")
            }
            Self::PassphraseRequired { path } => {
                write!(formatter, "the private key at '{path}' needs a passphrase")
            }
            Self::HostKeyMismatch {
                algorithm,
                stored,
                presented,
            } => write!(
                formatter,
                "the server identity changed: {algorithm} was {stored}, the server now presents {presented}"
            ),
            Self::UnknownHostKey {
                algorithm,
                fingerprint,
            } => write!(
                formatter,
                "the server identity is not known yet: {algorithm} {fingerprint}"
            ),
            Self::HostKeyRejected => formatter.write_str("the server identity was rejected"),
            Self::CredentialsRejected => {
                formatter.write_str("the password or passphrase was refused")
            }
            Self::ServerRejectedIdentity => {
                formatter.write_str("the server rejected the offered identity")
            }
        }
    }
}

impl std::error::Error for SecurityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_reach_formatted_output() {
        let password = SecretString::new("hunter2-correct-horse");

        assert_eq!(format!("{password:?}"), "SecretString(<redacted>)");
        assert!(!format!("{password:?}").contains(password.expose()));
        assert_eq!(password.expose(), "hunter2-correct-horse");
        assert!(!password.is_empty());
        assert_eq!(
            format!("{:?}", password.clone()),
            "SecretString(<redacted>)"
        );
    }

    #[test]
    fn security_error_text_carries_public_material_only() {
        let fingerprint = crate::sha256_fingerprint(&[7u8; 32]).expect("digest");
        let unknown = SecurityError::UnknownHostKey {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: fingerprint.clone(),
        };
        assert!(unknown.to_string().contains("ssh-ed25519"));
        assert!(unknown.to_string().contains(&fingerprint));
        assert!(unknown.is_retryable());
        assert_eq!(
            unknown.next_step(),
            "Compare the fingerprint, then accept or reject it."
        );

        let changed = SecurityError::HostKeyMismatch {
            algorithm: "ssh-ed25519".to_owned(),
            stored: "SHA256:old".to_owned(),
            presented: "SHA256:new".to_owned(),
        };
        let text = format!("{changed} {changed:?}");
        assert!(text.contains("SHA256:old"));
        assert!(text.contains("SHA256:new"));
        assert!(!changed.is_retryable());
        assert!(changed.next_step().starts_with("Do not continue."));
    }

    #[test]
    fn credential_errors_name_the_problem_without_the_secret() {
        let passphrase = SecretString::new("s3cret-passphrase");
        let error = SecurityError::CredentialsRejected;
        let rendered = format!("{error} {error:?} {}", error.next_step());

        assert!(rendered.contains("password or passphrase was refused"));
        assert!(!rendered.contains(passphrase.expose()));
        assert!(error.is_retryable());
    }

    #[test]
    fn agent_errors_say_which_agent_was_tried() {
        let error = SecurityError::AgentUnavailable {
            agent: AgentKind::OpenSshPipe,
        };
        assert_eq!(
            error.to_string(),
            "the Windows OpenSSH agent is not available"
        );
        assert_eq!(AgentKind::Pageant.label(), "Pageant");
        assert_eq!(
            SecurityError::HostKeyRejected.next_step(),
            "The connection stops here; choose another host if needed."
        );
        assert!(!SecurityError::HostKeyRejected.is_retryable());
    }
}
