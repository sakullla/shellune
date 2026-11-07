//! The remote connection form: which method, what was typed, and what happened (R3, R4, R6).
//!
//! The form is render-independent, like the host editor: it owns the method choice,
//! the text the user typed and the state of the attempt, and the view only draws
//! what it reports. Two rules matter here:
//!
//! * the secrets exist only in this form and in the [AuthMethod] built from it. They
//!   are never written to a host record, never formatted into a message and are
//!   handed over once, when the attempt starts;
//! * a failure is a value with the reason the transport gave and whether retrying
//!   can help, so the view cannot turn "the server identity changed" into an
//!   invitation to press the button again.

use std::time::{SystemTime, UNIX_EPOCH};

use shellune_core::{AgentKind, AuthMethod, HostConfig, SecretString};
use shellune_ssh::{ConnectError, HostKeyPrompt, KnownHostKey};
use shellune_storage::{HostKey, HostTrust, TrustEntry, TrustError};

use crate::app::copy::Strings;

/// Which authentication method the form is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodChoice {
    /// A password typed for this attempt.
    Password,
    /// A private key file that needs no passphrase.
    KeyFile,
    /// A private key file together with its passphrase.
    EncryptedKeyFile,
    /// A key the system agent holds.
    Agent,
}

impl MethodChoice {
    /// Every method, in the order the chooser shows them.
    pub const ALL: [Self; 4] = [
        Self::Password,
        Self::KeyFile,
        Self::EncryptedKeyFile,
        Self::Agent,
    ];

    /// Label of the method, in the current language.
    #[must_use]
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::Password => strings.auth_password,
            Self::KeyFile => strings.auth_key,
            Self::EncryptedKeyFile => strings.auth_key_passphrase,
            Self::Agent => strings.auth_agent,
        }
    }

    /// The field the method needs, in the words of the form.
    #[must_use]
    pub fn hint(self, strings: &Strings) -> &'static str {
        match self {
            Self::Password => strings.auth_password_hint,
            Self::KeyFile => strings.auth_key_hint,
            Self::EncryptedKeyFile => strings.auth_key_passphrase_hint,
            Self::Agent => strings.auth_agent_hint,
        }
    }

    /// Debug selector of the chooser entry, used by the tests.
    #[must_use]
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::Password => "remote.method.password",
            Self::KeyFile => "remote.method.key",
            Self::EncryptedKeyFile => "remote.method.key-passphrase",
            Self::Agent => "remote.method.agent",
        }
    }

    /// Whether the method reads a key file path.
    #[must_use]
    pub fn needs_key_path(self) -> bool {
        matches!(self, Self::KeyFile | Self::EncryptedKeyFile)
    }

    /// Whether the method reads a passphrase.
    #[must_use]
    pub fn needs_passphrase(self) -> bool {
        matches!(self, Self::EncryptedKeyFile)
    }

    /// Whether the method reads a password.
    #[must_use]
    pub fn needs_password(self) -> bool {
        matches!(self, Self::Password)
    }

    /// The choice that stands for a domain method kind.
    #[must_use]
    pub fn from_kind(kind: shellune_core::AuthMethodKind) -> Self {
        match kind {
            shellune_core::AuthMethodKind::Password => Self::Password,
            shellune_core::AuthMethodKind::KeyFile => Self::KeyFile,
            shellune_core::AuthMethodKind::EncryptedKeyFile => Self::EncryptedKeyFile,
            shellune_core::AuthMethodKind::Agent => Self::Agent,
        }
    }

    /// The domain method kind this choice stands for.
    #[must_use]
    pub fn kind(self) -> shellune_core::AuthMethodKind {
        match self {
            Self::Password => shellune_core::AuthMethodKind::Password,
            Self::KeyFile => shellune_core::AuthMethodKind::KeyFile,
            Self::EncryptedKeyFile => shellune_core::AuthMethodKind::EncryptedKeyFile,
            Self::Agent => shellune_core::AuthMethodKind::Agent,
        }
    }

    /// The method that matches a stored key path, used when a host carries one.
    #[must_use]
    pub fn for_stored_key() -> Self {
        Self::KeyFile
    }
}

/// Why the last attempt failed, and whether trying again can help.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFailure {
    reason: String,
    next_step: String,
    retryable: bool,
}

impl RemoteFailure {
    /// A failure with the reason the transport reported.
    #[must_use]
    pub fn from_error(error: &ConnectError) -> Self {
        Self {
            reason: error.to_string(),
            next_step: error.next_step().to_owned(),
            retryable: error.is_retryable(),
        }
    }

    /// A failure the shell itself produced, such as a trust record that could not be
    /// written.
    #[must_use]
    pub fn new(reason: impl Into<String>, next_step: impl Into<String>, retryable: bool) -> Self {
        Self {
            reason: reason.into(),
            next_step: next_step.into(),
            retryable,
        }
    }

    /// What went wrong, in the words of the layer that knows.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// What the user can do next.
    #[must_use]
    pub fn next_step(&self) -> &str {
        &self.next_step
    }

    /// Whether offering the form again is honest.
    #[must_use]
    pub fn retryable(&self) -> bool {
        self.retryable
    }
}

/// Where an attempt currently is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteStatus {
    /// The form is filled in and nothing has been tried.
    Idle,
    /// Waiting for the transport.
    Connecting,
    /// The server identity is unknown and the user has to decide.
    VerifyingHost(HostKeyPrompt),
    /// The remote shell is live.
    Live,
    /// The attempt ended, with the reason.
    Failed(RemoteFailure),
}

impl RemoteStatus {
    /// Whether an attempt is in flight.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Connecting)
    }

    /// Whether the user is being asked about the server identity.
    #[must_use]
    pub fn is_asking(&self) -> bool {
        matches!(self, Self::VerifyingHost(_))
    }

    /// The prompt, when there is one.
    #[must_use]
    pub fn prompt(&self) -> Option<&HostKeyPrompt> {
        match self {
            Self::VerifyingHost(prompt) => Some(prompt),
            _ => None,
        }
    }

    /// The failure, when the attempt ended badly.
    #[must_use]
    pub fn failure(&self) -> Option<&RemoteFailure> {
        match self {
            Self::Failed(failure) => Some(failure),
            _ => None,
        }
    }
}

/// Everything the remote connection panel holds.
#[derive(Debug)]
pub struct RemotePanel {
    host: HostConfig,
    method: MethodChoice,
    password: String,
    key_path: String,
    passphrase: String,
    status: RemoteStatus,
    form_error: Option<String>,
}

impl RemotePanel {
    /// A form for one saved host.
    ///
    /// A host that already carries a key path starts on the key method with that
    /// path filled in, so an imported identity file is used instead of retyped.
    #[must_use]
    pub fn new(host: &HostConfig) -> Self {
        let key_path = host.identity_file().unwrap_or_default().to_owned();
        // The method chosen in the host form wins; a host saved before that choice
        // existed is guessed from whether it carries a key path, as before.
        let method = match host.auth() {
            Some(kind) => MethodChoice::from_kind(kind),
            None if key_path.is_empty() => MethodChoice::Password,
            None => MethodChoice::for_stored_key(),
        };
        Self {
            host: host.clone(),
            method,
            password: String::new(),
            key_path,
            passphrase: String::new(),
            status: RemoteStatus::Idle,
            form_error: None,
        }
    }

    /// The host this form connects to.
    #[must_use]
    pub fn host(&self) -> &HostConfig {
        &self.host
    }

    /// The chosen method.
    #[must_use]
    pub fn method(&self) -> MethodChoice {
        self.method
    }

    /// Chooses a method, keeping what was already typed for it.
    pub fn select_method(&mut self, method: MethodChoice) {
        if self.method != method {
            self.method = method;
            // A method change is a new attempt, so the previous failure no longer
            // describes what is on screen.
            self.form_error = None;
            if matches!(self.status, RemoteStatus::Failed(_)) {
                self.status = RemoteStatus::Idle;
            }
        }
    }

    /// The typed password.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }

    /// Replaces the typed password.
    pub fn set_password(&mut self, text: impl Into<String>) {
        self.password = text.into();
    }

    /// The key file path.
    #[must_use]
    pub fn key_path(&self) -> &str {
        &self.key_path
    }

    /// Replaces the key file path.
    pub fn set_key_path(&mut self, text: impl Into<String>) {
        self.key_path = text.into();
    }

    /// The typed passphrase.
    #[must_use]
    pub fn passphrase(&self) -> &str {
        &self.passphrase
    }

    /// Replaces the typed passphrase.
    pub fn set_passphrase(&mut self, text: impl Into<String>) {
        self.passphrase = text.into();
    }

    /// Where the attempt is.
    #[must_use]
    pub fn status(&self) -> &RemoteStatus {
        &self.status
    }

    /// Replaces the status.
    pub fn set_status(&mut self, status: RemoteStatus) {
        self.status = status;
    }

    /// Records a failure.
    pub fn fail(&mut self, failure: RemoteFailure) {
        self.status = RemoteStatus::Failed(failure);
    }

    /// Why the form itself refused to start an attempt.
    #[must_use]
    pub fn form_error(&self) -> Option<&str> {
        self.form_error.as_deref()
    }

    /// Reports that the form cannot produce credentials yet.
    pub fn set_form_error(&mut self, reason: impl Into<String>) {
        self.form_error = Some(reason.into());
    }

    /// Drops everything typed, which is what closing the panel does.
    pub fn clear_secrets(&mut self) {
        self.password.clear();
        self.passphrase.clear();
        self.form_error = None;
    }

    /// The credentials of one attempt, built from what is on screen.
    ///
    /// The typed password and passphrase are moved into [AuthMethod], so a second
    /// attempt has to be typed again instead of silently reusing a secret the user
    /// may have meant for one try only.
    ///
    /// # Errors
    ///
    /// Returns the reason a field is missing, naming the field rather than the value.
    pub fn take_auth(&mut self, strings: &Strings) -> Result<AuthMethod, String> {
        let auth = match self.method {
            MethodChoice::Password => {
                if self.password.is_empty() {
                    return Err(strings.auth_missing_password.to_owned());
                }
                AuthMethod::password(std::mem::take(&mut self.password))
            }
            MethodChoice::KeyFile | MethodChoice::EncryptedKeyFile => {
                if self.key_path.trim().is_empty() {
                    return Err(strings.auth_missing_key.to_owned());
                }
                let passphrase = if self.method.needs_passphrase() {
                    if self.passphrase.is_empty() {
                        return Err(strings.auth_missing_passphrase.to_owned());
                    }
                    Some(SecretString::new(std::mem::take(&mut self.passphrase)))
                } else {
                    None
                };
                AuthMethod::key_file(self.key_path.trim().to_owned(), passphrase)
            }
            MethodChoice::Agent => AuthMethod::agent(default_agent()),
        };
        self.form_error = None;
        Ok(auth)
    }
}

/// The agent this platform starts with.
#[must_use]
pub fn default_agent() -> AgentKind {
    shellune_ssh::agent::default_kind()
}

/// What the trust records say about a host, before anything is dialled.
///
/// A stored record means "this fingerprint was accepted", so the connection may
/// proceed; the transport still compares what the server presents and reports a
/// change instead of connecting. No record means the user has to be asked.
#[must_use]
pub fn known_host_key(trust: &HostTrust, host: &HostConfig) -> KnownHostKey {
    match trust.entry(host.address(), host.port().get()) {
        Some(entry) => KnownHostKey::Trusted {
            fingerprint: entry.fingerprint().to_owned(),
        },
        None => KnownHostKey::Unknown,
    }
}

/// Stores an identity the user accepted at the prompt.
///
/// # Errors
///
/// Returns [TrustError] when the value cannot be stored, including the case where a
/// different identity is already trusted for this host: the shell then reports that
/// and does not connect.
pub fn accept_prompted_identity(
    trust: &mut HostTrust,
    prompt: &HostKeyPrompt,
    at_unix: u64,
) -> Result<TrustEntry, TrustError> {
    let key = HostKey::new(
        prompt.presented.algorithm.clone(),
        prompt.presented.fingerprint.clone(),
        prompt.presented.public_key.clone(),
    )?;
    trust.accept(&prompt.host, prompt.port, &key, at_unix)?;
    trust
        .entry(&prompt.host, prompt.port)
        .cloned()
        .ok_or(TrustError::Invalid {
            reason: "the accepted identity is not in the trust library".to_owned(),
        })
}

/// Seconds since the Unix epoch, for the records that carry a time.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::copy::{CHINESE, ENGLISH};
    use shellune_ssh::PresentedKey;

    fn host() -> HostConfig {
        HostConfig::new("web", "10.0.0.7", 22, "deploy").expect("a valid host")
    }

    fn prompt() -> HostKeyPrompt {
        HostKeyPrompt {
            host: "10.0.0.7".to_owned(),
            port: 22,
            presented: PresentedKey {
                algorithm: "ssh-ed25519".to_owned(),
                fingerprint: shellune_core::sha256_fingerprint(&[5u8; 32]).expect("a digest"),
                public_key: "AAAAC3NzaC1lZDI1NTE5".to_owned(),
            },
        }
    }

    #[test]
    fn a_host_with_a_saved_key_path_starts_on_the_key_method() {
        let mut saved = host();
        saved.set_identity_file(Some("/home/deploy/.ssh/id_ed25519"));
        let panel = RemotePanel::new(&saved);
        assert_eq!(panel.method(), MethodChoice::KeyFile);
        assert_eq!(panel.key_path(), "/home/deploy/.ssh/id_ed25519");
        assert_eq!(panel.status(), &RemoteStatus::Idle);

        let plain = RemotePanel::new(&host());
        assert_eq!(plain.method(), MethodChoice::Password);
        assert_eq!(plain.key_path(), "");
    }

    #[test]
    fn every_method_says_which_field_it_needs() {
        let labels: Vec<&str> = MethodChoice::ALL
            .iter()
            .map(|method| method.label(&ENGLISH))
            .collect();
        assert_eq!(labels.len(), 4);
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "each method has its own label");

        assert!(MethodChoice::Password.needs_password());
        assert!(MethodChoice::KeyFile.needs_key_path());
        assert!(MethodChoice::EncryptedKeyFile.needs_passphrase());
        assert!(!MethodChoice::Agent.needs_key_path());
        assert_eq!(
            MethodChoice::Agent.label(&CHINESE),
            CHINESE.auth_agent,
            "the method names are translated"
        );
    }

    #[test]
    fn a_form_without_credentials_refuses_to_start_and_names_the_field() {
        let mut panel = RemotePanel::new(&host());
        assert_eq!(
            panel
                .take_auth(&ENGLISH)
                .expect_err("no password was typed"),
            ENGLISH.auth_missing_password
        );

        panel.select_method(MethodChoice::EncryptedKeyFile);
        assert_eq!(
            panel
                .take_auth(&ENGLISH)
                .expect_err("no key path was typed"),
            ENGLISH.auth_missing_key
        );

        panel.set_key_path("/keys/work");
        assert_eq!(
            panel
                .take_auth(&ENGLISH)
                .expect_err("no passphrase was typed"),
            ENGLISH.auth_missing_passphrase
        );
        assert!(
            panel.form_error().is_none(),
            "a refused form is not a connection failure"
        );
    }

    #[test]
    fn the_typed_secret_is_handed_over_once_and_not_kept() {
        let mut panel = RemotePanel::new(&host());
        panel.set_password("hunter2-correct-horse");
        let auth = panel.take_auth(&ENGLISH).expect("a password is present");
        assert_eq!(auth.kind(), shellune_core::AuthMethodKind::Password);
        assert!(
            panel.password().is_empty(),
            "the form keeps no copy of the secret after the attempt starts"
        );
        assert!(
            !format!("{auth:?}").contains("hunter2"),
            "the credentials never appear in formatted output"
        );
    }

    #[test]
    fn a_method_change_clears_a_failure_that_belonged_to_the_previous_one() {
        let mut panel = RemotePanel::new(&host());
        panel.fail(RemoteFailure::new("wrong password", "try again", true));
        assert!(panel.status().failure().is_some());

        panel.select_method(MethodChoice::Agent);
        assert_eq!(panel.method(), MethodChoice::Agent);
        assert_eq!(
            panel.status(),
            &RemoteStatus::Idle,
            "the failure described the password attempt, not the agent one"
        );
    }

    #[test]
    fn a_failure_carries_the_reason_the_next_step_and_whether_retrying_helps() {
        let changed = ConnectError::HostKeyChanged {
            stored: "SHA256:old".to_owned(),
            presented: prompt().presented,
        };
        let failure = RemoteFailure::from_error(&changed);
        assert!(failure.reason().contains("SHA256:old"));
        assert!(failure.next_step().starts_with("Do not continue."));
        assert!(!failure.retryable());

        let rejected = ConnectError::AuthRejected;
        let failure = RemoteFailure::from_error(&rejected);
        assert!(failure.retryable(), "another method can still work");
        assert!(failure.next_step().contains("authentication method"));
    }

    #[test]
    fn a_stored_identity_is_offered_as_trusted_and_a_missing_one_as_unknown() {
        let host = host();
        let trust = HostTrust::new();
        assert_eq!(known_host_key(&trust, &host), KnownHostKey::Unknown);

        let mut trust = HostTrust::new();
        let accepted = HostKey::new(
            "ssh-ed25519",
            prompt().presented.fingerprint,
            "AAAAC3NzaC1lZDI1NTE5",
        )
        .expect("a comparable identity");
        trust
            .accept("10.0.0.7", 22, &accepted, 1)
            .expect("accepted");
        assert_eq!(
            known_host_key(&trust, &host),
            KnownHostKey::Trusted {
                fingerprint: accepted.fingerprint,
            }
        );
    }

    #[test]
    fn accepting_a_prompted_identity_stores_it_and_a_change_is_refused() {
        let mut trust = HostTrust::new();
        let prompt = prompt();
        let entry = accept_prompted_identity(&mut trust, &prompt, 1_700_000_000)
            .expect("the first identity is accepted");
        assert_eq!(entry.fingerprint(), prompt.presented.fingerprint);
        assert_eq!(entry.accepted_unix(), 1_700_000_000);

        let mut changed = prompt.clone();
        changed.presented.fingerprint =
            shellune_core::sha256_fingerprint(&[9u8; 32]).expect("a digest");
        let error = accept_prompted_identity(&mut trust, &changed, 1_700_000_100)
            .expect_err("a changed identity must not be stored");
        assert!(matches!(error, TrustError::IdentityChanged { .. }));
        assert_eq!(
            trust.entry("10.0.0.7", 22).expect("stored").fingerprint(),
            prompt.presented.fingerprint,
            "the accepted identity is still the stored one"
        );
    }

    #[test]
    fn the_clock_a_record_uses_is_a_real_instant() {
        assert!(now_unix() > 1_700_000_000, "the clock has moved on");
    }
}
