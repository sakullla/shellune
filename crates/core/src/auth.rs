//! How one connection attempt proves who the user is (R4).
//!
//! A method is created for a single attempt and dropped with it: the password and
//! the key passphrase live in [`SecretString`] and never become part of a host
//! record, and the key file is only ever named by its path. That is what keeps the
//! authentication path from being a way for a secret to reach storage, a log line or
//! an error message.

use crate::error::{AgentKind, SecretString};

/// Which kind of authentication an attempt uses.
///
/// The kind is what the shell can show, log and remember as a preference; it holds
/// no secret, which is why it is safe to put in a message or a test assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethodKind {
    /// A password typed for this attempt.
    Password,
    /// A private key file that needs no passphrase.
    KeyFile,
    /// A private key file protected by a passphrase.
    EncryptedKeyFile,
    /// A key held by the system's SSH agent.
    Agent,
}

impl AuthMethodKind {
    /// Every kind, in the order a chooser shows them.
    pub const ALL: [Self; 4] = [
        Self::Password,
        Self::KeyFile,
        Self::EncryptedKeyFile,
        Self::Agent,
    ];

    /// Label used in the chooser and in log lines.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Password => "Password",
            Self::KeyFile => "Key file",
            Self::EncryptedKeyFile => "Key file with passphrase",
            Self::Agent => "System agent",
        }
    }

    /// Whether the kind needs a key file path.
    #[must_use]
    pub fn needs_key_path(self) -> bool {
        matches!(self, Self::KeyFile | Self::EncryptedKeyFile)
    }

    /// Whether the kind needs a passphrase next to the key path.
    #[must_use]
    pub fn needs_passphrase(self) -> bool {
        matches!(self, Self::EncryptedKeyFile)
    }

    /// Whether the kind needs a password.
    #[must_use]
    pub fn needs_password(self) -> bool {
        matches!(self, Self::Password)
    }
}

/// The credentials of one connection attempt.
///
/// Deliberately not `Clone`: passing an attempt on moves it, so a secret is not
/// copied into a second place that would outlive the attempt.
#[derive(Debug)]
pub enum AuthMethod {
    /// A password, used once and zeroed when the attempt ends.
    Password {
        /// The typed password.
        password: SecretString,
    },
    /// A private key file, optionally protected by a passphrase.
    KeyFile {
        /// Path of the key file. Only the path is kept anywhere.
        path: String,
        /// The passphrase, when the file is encrypted.
        passphrase: Option<SecretString>,
    },
    /// A key the system agent holds.
    Agent {
        /// Which agent is used, so an unavailable one can be named.
        kind: AgentKind,
    },
}

impl AuthMethod {
    /// A password attempt.
    #[must_use]
    pub fn password(password: impl Into<String>) -> Self {
        Self::Password {
            password: SecretString::new(password),
        }
    }

    /// A key file attempt, encrypted when a passphrase is given.
    #[must_use]
    pub fn key_file(path: impl Into<String>, passphrase: Option<SecretString>) -> Self {
        Self::KeyFile {
            path: path.into(),
            passphrase,
        }
    }

    /// An agent attempt.
    #[must_use]
    pub fn agent(kind: AgentKind) -> Self {
        Self::Agent { kind }
    }

    /// Which kind of method this is.
    #[must_use]
    pub fn kind(&self) -> AuthMethodKind {
        match self {
            Self::Password { .. } => AuthMethodKind::Password,
            Self::KeyFile { passphrase, .. } => match passphrase {
                Some(_) => AuthMethodKind::EncryptedKeyFile,
                None => AuthMethodKind::KeyFile,
            },
            Self::Agent { .. } => AuthMethodKind::Agent,
        }
    }

    /// The key path, when the method uses one.
    #[must_use]
    pub fn key_path(&self) -> Option<&str> {
        match self {
            Self::KeyFile { path, .. } => Some(path),
            Self::Password { .. } | Self::Agent { .. } => None,
        }
    }

    /// The agent kind, when the method uses one.
    #[must_use]
    pub fn agent_kind(&self) -> Option<AgentKind> {
        match self {
            Self::Agent { kind } => Some(*kind),
            Self::Password { .. } | Self::KeyFile { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_reports_what_it_needs_without_revealing_it() {
        let password = AuthMethod::password("hunter2-correct-horse");
        assert_eq!(password.kind(), AuthMethodKind::Password);
        assert!(password.kind().needs_password());

        let rendered = format!("{password:?}");
        assert!(!rendered.contains("hunter2-correct-horse"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");

        let plain = AuthMethod::key_file("/home/deploy/.ssh/id_ed25519", None);
        assert_eq!(plain.kind(), AuthMethodKind::KeyFile);
        assert_eq!(plain.key_path(), Some("/home/deploy/.ssh/id_ed25519"));
        assert!(!plain.kind().needs_passphrase());

        let encrypted = AuthMethod::key_file(
            "/home/deploy/.ssh/id_ed25519",
            Some(SecretString::new("key-passphrase")),
        );
        assert_eq!(encrypted.kind(), AuthMethodKind::EncryptedKeyFile);
        assert!(
            encrypted.kind().needs_passphrase(),
            "an encrypted key asks for a passphrase"
        );
        assert!(!format!("{encrypted:?}").contains("key-passphrase"));

        let agent = AuthMethod::agent(AgentKind::OpenSshPipe);
        assert_eq!(agent.kind(), AuthMethodKind::Agent);
        assert_eq!(agent.agent_kind(), Some(AgentKind::OpenSshPipe));
        assert_eq!(agent.kind().label(), "System agent");
    }

    #[test]
    fn every_kind_says_what_it_is_and_a_kind_alone_is_not_a_secret() {
        let labels: Vec<&str> = AuthMethodKind::ALL
            .iter()
            .map(|kind| kind.label())
            .collect();
        assert_eq!(labels.len(), 4);
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "each kind has its own label");
        assert!(AuthMethodKind::Password.needs_password());
        assert!(AuthMethodKind::KeyFile.needs_key_path());
        assert!(AuthMethodKind::EncryptedKeyFile.needs_key_path());
        assert!(!AuthMethodKind::Agent.needs_key_path());
    }
}
