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

use std::collections::HashMap;

use shellune_core::{
    AgentKind, AuthMethod, AuthMethodKind, HostConfig, HostId, SecretString, X11Forwarding,
    connection_jump_fault,
};
use shellune_ssh::{
    ChainEvent, ChainFailure, ChainRequest, ChainSession, ConnectError, FileLink, HopAuth,
    HostKeyPrompt, KnownHostKey, PtySize, RemoteEvent, RemoteSession, TransferLink, X11Status,
};
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

    /// A jump-chain failure. The reason is the chain's own text, which names the hop.
    ///
    /// [`ChainFailure::session_failure`] drops that hop: an authentication failure
    /// becomes [`shellune_core::SessionFailure::AuthenticationFailed`], and a disconnect
    /// becomes a remote I/O failure whose host is "the server". The panel keeps the
    /// chain text instead.
    #[must_use]
    pub fn from_chain(failure: &ChainFailure) -> Self {
        let (next_step, retryable) = match failure {
            ChainFailure::Hop { error, .. } => (error.next_step().to_owned(), error.is_retryable()),
            ChainFailure::NotDialed { .. } => (
                "Fix the jump chain or the ProxyCommand, then connect again.".to_owned(),
                true,
            ),
            ChainFailure::Cancelled { .. } => {
                ("Connect again to start a new attempt.".to_owned(), true)
            }
            ChainFailure::ProxyNotConfirmed { .. } => (
                "Confirm the command to run it, or remove the ProxyCommand from the host."
                    .to_owned(),
                true,
            ),
        };
        Self {
            reason: failure.to_string(),
            next_step,
            retryable,
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

/// What the user answered when asked to accept the risks of trusted X11 forwarding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X11Answer {
    /// Trusted forwarding, for this connection only.
    Trusted,
    /// Keep forwarding, but with the SECURITY extension limits (`ssh -X`).
    Restricted,
    /// Connect nothing at all; the form stays as it was.
    Cancel,
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
    /// A ProxyCommand is expanded and will not run until the user confirms it.
    ConfirmingProxy {
        /// The hop the command would reach.
        host_id: HostId,
        /// How that hop is named in the form.
        label: String,
        /// The expanded command. Nothing has been started.
        command: String,
    },
    /// Trusted X11 forwarding is chosen and waitng for the user to accept its risks.
    ///
    /// Nothing has been dialled: a trusted display channel is only asked for after
    /// this question was answered, every time a connection uses it.
    ConfirmingX11 {
        /// The mode that was chosen and needs the answer.
        mode: X11Forwarding,
    },
    /// The remote shell is live.
    Live,
    /// The attempt ended, with the reason.
    Failed(RemoteFailure),
}

impl RemoteStatus {
    /// Whether an attempt is in flight.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::ConfirmingProxy { .. } | Self::ConfirmingX11 { .. }
        )
    }

    /// Whether the user is being asked about the server identity or a ProxyCommand.
    #[must_use]
    pub fn is_asking(&self) -> bool {
        matches!(
            self,
            Self::VerifyingHost(_) | Self::ConfirmingProxy { .. } | Self::ConfirmingX11 { .. }
        )
    }

    /// The X11 mode waiting for its risk confirmation, when one is.
    #[must_use]
    pub fn x11_question(&self) -> Option<X11Forwarding> {
        match self {
            Self::ConfirmingX11 { mode } => Some(*mode),
            _ => None,
        }
    }

    /// The prompt, when there is one.
    #[must_use]
    pub fn prompt(&self) -> Option<&HostKeyPrompt> {
        match self {
            Self::VerifyingHost(prompt) => Some(prompt),
            _ => None,
        }
    }

    /// The ProxyCommand waiting for a yes or a no.
    #[must_use]
    pub fn proxy_question(&self) -> Option<(HostId, &str, &str)> {
        match self {
            Self::ConfirmingProxy {
                host_id,
                label,
                command,
            } => Some((*host_id, label.as_str(), command.as_str())),
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
    /// Names the hop this form is collecting credentials for, when it is not obvious.
    hop_label: Option<String>,
    /// How this connection forwards X11, starting from the host's default (R11).
    ///
    /// It belongs to this attempt: choosing another mode here does not change the
    /// saved host, which is what the host dialog is for.
    x11: X11Forwarding,
    /// Whether the trusted mode's risks were accepted for this attempt.
    ///
    /// Cleared for every new attempt, because the confirmation is per connection.
    x11_accepted: bool,
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
            hop_label: None,
            x11: host.x11(),
            x11_accepted: false,
        }
    }

    /// The hop this form is asking about, when the chain has more than one node.
    #[must_use]
    pub fn hop_label(&self) -> Option<&str> {
        self.hop_label.as_deref()
    }

    /// Sets the hop label drawn above the fields.
    pub fn set_hop_label(&mut self, label: impl Into<String>) {
        let label = label.into();
        self.hop_label = (!label.is_empty()).then_some(label);
    }

    /// The host this form connects to.
    #[must_use]
    pub fn host(&self) -> &HostConfig {
        &self.host
    }

    /// How this connection forwards X11, as the form is set now (R11).
    #[must_use]
    pub fn x11(&self) -> X11Forwarding {
        self.x11
    }

    /// Chooses the X11 mode for this attempt, returning whether it changed.
    ///
    /// The mode is not written to the host record: it applies to this connection, and
    /// the host dialog is where a default is saved.
    pub fn set_x11(&mut self, mode: X11Forwarding) -> bool {
        let changed = self.x11 != mode;
        self.x11 = mode;
        if changed {
            // A different mode is a different question, so an earlier answer no longer
            // describes what the form is asking for.
            self.x11_accepted = false;
        }
        changed
    }

    /// Whether the trusted mode of this form was accepted for this attempt.
    #[allow(dead_code)]
    #[must_use]
    pub fn x11_accepted(&self) -> bool {
        self.x11_accepted
    }

    /// Records that the risks of trusted forwarding were accepted.
    pub fn accept_x11(&mut self) {
        self.x11_accepted = true;
    }

    /// Asks again before the next attempt: the confirmation is per connection.
    pub fn forget_x11_acceptance(&mut self) {
        self.x11_accepted = false;
    }

    /// Whether this form asks for a risk confirmation before it may dial.
    #[must_use]
    pub fn needs_x11_confirmation(&self) -> bool {
        self.x11.needs_risk_confirmation() && !self.x11_accepted
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

/// Credentials a saved host can supply without asking, when they are already complete.
///
/// Agent and a key path need nothing typed. A password or passphrase is complete only
/// when `secret` holds it. Anything missing returns `None` so the form can ask for
/// that hop alone.
#[must_use]
pub fn stored_auth(host: &HostConfig, secret: Option<&SecretString>) -> Option<AuthMethod> {
    match host.auth()? {
        AuthMethodKind::Agent => Some(AuthMethod::agent(default_agent())),
        AuthMethodKind::KeyFile => {
            let path = host.identity_file()?.trim();
            (!path.is_empty()).then(|| AuthMethod::key_file(path.to_owned(), None))
        }
        AuthMethodKind::Password => {
            let text = secret?.expose();
            (!text.is_empty()).then(|| AuthMethod::password(text.to_owned()))
        }
        AuthMethodKind::EncryptedKeyFile => {
            let path = host.identity_file()?.trim();
            let text = secret?.expose();
            if path.is_empty() || text.is_empty() {
                None
            } else {
                Some(AuthMethod::key_file(
                    path.to_owned(),
                    Some(SecretString::new(text.to_owned())),
                ))
            }
        }
    }
}

/// The ordered hops of one connection, plus the credentials collected so far.
///
/// Building this does not dial and does not start a ProxyCommand. A cycle, a missing
/// hop, or a host that has both a jump list and a ProxyCommand is a [`Self::fault`]
/// and leaves `order` empty.
pub struct ChainDraft {
    hosts: Vec<HostConfig>,
    target: HostId,
    order: Vec<HostId>,
    auths: Vec<HopAuth>,
    fault: Option<String>,
}

impl ChainDraft {
    /// The chain from `target` through `saved`.
    ///
    /// `target` is added when it is not already in `saved`, which is how a quick
    /// connection dials a host that has not been written.
    #[must_use]
    pub fn prepare(saved: &[HostConfig], target: &HostConfig) -> Self {
        let mut hosts = saved.to_vec();
        if !hosts.iter().any(|host| host.id() == target.id()) {
            hosts.push(target.clone());
        }
        let fault = connection_jump_fault(&hosts, target.id())
            .map(|fault| fault.message().to_owned())
            .or_else(|| chain_shape_fault(&hosts, target.id()));
        let order = if fault.is_some() {
            Vec::new()
        } else {
            expand_order(&hosts, target.id()).unwrap_or_default()
        };
        Self {
            hosts,
            target: target.id(),
            order,
            auths: Vec::new(),
            fault,
        }
    }

    /// A chain of one host that is not in the saved book.
    #[must_use]
    pub fn single(host: HostConfig) -> Self {
        Self::prepare(std::slice::from_ref(&host), &host)
    }

    /// Why this chain must not be dialled.
    #[must_use]
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// Hops in dial order. Empty when [`Self::fault`] is set.
    #[must_use]
    pub fn order(&self) -> &[HostId] {
        &self.order
    }

    /// The host the session is for.
    #[must_use]
    pub fn target_host(&self) -> Option<&HostConfig> {
        self.hosts.iter().find(|host| host.id() == self.target)
    }

    /// One hop's saved record.
    #[must_use]
    pub fn host(&self, id: HostId) -> Option<&HostConfig> {
        self.hosts.iter().find(|host| host.id() == id)
    }

    /// Whether this hop already has the credentials the chain will use.
    #[must_use]
    pub fn has_auth(&self, id: HostId) -> bool {
        self.auths.iter().any(|auth| auth.host_id == id)
    }

    /// The first hop that still needs credentials.
    #[must_use]
    pub fn next_needed(&self) -> Option<HostId> {
        self.order.iter().copied().find(|id| !self.has_auth(*id))
    }

    /// Records one hop's credentials. A second set for the same hop is ignored.
    pub fn push_auth(&mut self, auth: HopAuth) {
        if !self.has_auth(auth.host_id) {
            self.auths.push(auth);
        }
    }

    /// The pieces [`ChainRequest::new`] needs. The draft is consumed.
    #[must_use]
    pub fn into_parts(self) -> (Vec<HostConfig>, HostId, Vec<HopAuth>) {
        (self.hosts, self.target, self.auths)
    }

    /// A request for this chain, when every hop has credentials and nothing is wrong.
    #[must_use]
    pub fn into_request(self, size: PtySize) -> Option<ChainRequest> {
        if self.fault.is_some() || self.next_needed().is_some() {
            return None;
        }
        let (hosts, target, auth) = self.into_parts();
        Some(ChainRequest::new(hosts, target, auth, size))
    }
}

fn expand_order(hosts: &[HostConfig], target: HostId) -> Result<Vec<HostId>, String> {
    let by_id: HashMap<HostId, &HostConfig> = hosts.iter().map(|host| (host.id(), host)).collect();
    let mut order = Vec::new();
    let mut stack = Vec::new();
    expand_hops(target, &by_id, &mut order, &mut stack)?;
    Ok(order)
}

fn chain_shape_fault(hosts: &[HostConfig], target: HostId) -> Option<String> {
    let order = match expand_order(hosts, target) {
        Ok(order) => order,
        Err(message) => return Some(message),
    };
    let by_id: HashMap<HostId, &HostConfig> = hosts.iter().map(|host| (host.id(), host)).collect();
    for id in order {
        if let Some(host) = by_id.get(&id)
            && !host.jumps().is_empty()
            && host.proxy_command().is_some()
        {
            return Some(format!(
                "\"{}\" has both a jump chain and a ProxyCommand. Keep one way to reach it and remove the other before connecting. Nothing was connected and no command was run.",
                host.name()
            ));
        }
    }
    None
}

fn expand_hops(
    id: HostId,
    by_id: &HashMap<HostId, &HostConfig>,
    out: &mut Vec<HostId>,
    stack: &mut Vec<HostId>,
) -> Result<(), String> {
    if out.contains(&id) {
        return Ok(());
    }
    if stack.contains(&id) {
        return Err(
            "the jump chain loops. Remove the hop that returns to an earlier host before connecting. Nothing was connected and no command was run."
                .to_owned(),
        );
    }
    let Some(host) = by_id.get(&id) else {
        return Err(format!(
            "host {id} is not saved, so it cannot be connected. Nothing was connected and no command was run."
        ));
    };
    stack.push(id);
    for hop in host.jumps() {
        expand_hops(*hop, by_id, out, stack)?;
    }
    stack.pop();
    if !out.contains(&id) {
        out.push(id);
    }
    Ok(())
}

/// What one attempt's transport reported, whether it is a direct session or a chain.
pub enum LinkEvent {
    /// An unknown identity on one hop.
    HostKey {
        /// The hop, when the event names one.
        host_id: Option<HostId>,
        /// The question.
        prompt: HostKeyPrompt,
    },
    /// A ProxyCommand waiting for confirmation.
    Proxy {
        /// The hop the command would reach.
        host_id: HostId,
        /// How that hop is named.
        label: String,
        /// The expanded command.
        command: String,
    },
    /// The hop accepted its credentials.
    HopReady(HostId),
    /// The target shell is running.
    Connected,
    /// Bytes from the target shell.
    Data(Vec<u8>),
    /// The target shell ended.
    Exited {
        /// Exit status, when one was reported.
        code: Option<u32>,
    },
    /// A direct session failed.
    FailedRemote(ConnectError),
    /// What X11 forwarding of this attempt is doing (R11).
    ///
    /// Not a connection outcome: a failed display bridge leaves the shell working.
    X11 {
        /// The hop whose session it belongs to, when the event names one.
        host_id: Option<HostId>,
        /// Off, enabling, ready or failed.
        status: X11Status,
    },
    /// A chain failed. The display text names the hop.
    FailedChain(ChainFailure),
}

/// The transport behind one attempt.
pub enum AttemptLink {
    /// A scripted session. Real dials use [`Self::Chain`].
    #[cfg_attr(not(test), allow(dead_code))]
    Remote(RemoteSession),
    /// A jump chain, including a chain of one host.
    Chain(ChainSession),
}

impl AttemptLink {
    /// Whether this transport is a jump chain.
    #[must_use]
    pub fn is_chain(&self) -> bool {
        matches!(self, Self::Chain(_))
    }

    /// Stops the attempt.
    pub fn close(&mut self) {
        match self {
            Self::Remote(session) => session.close(),
            Self::Chain(session) => session.close(),
        }
    }

    /// Events waiting since the last drain.
    pub fn drain(&mut self) -> Vec<LinkEvent> {
        match self {
            Self::Remote(session) => session
                .drain()
                .into_iter()
                .map(|event| match event {
                    RemoteEvent::HostKeyPrompt(prompt) => LinkEvent::HostKey {
                        host_id: None,
                        prompt,
                    },
                    RemoteEvent::Connected => LinkEvent::Connected,
                    RemoteEvent::X11(status) => LinkEvent::X11 {
                        host_id: None,
                        status,
                    },
                    RemoteEvent::Data(bytes) => LinkEvent::Data(bytes),
                    RemoteEvent::Exited { code } => LinkEvent::Exited { code },
                    RemoteEvent::Failed(error) => LinkEvent::FailedRemote(error),
                })
                .collect(),
            Self::Chain(session) => session
                .drain()
                .into_iter()
                .filter_map(|event| match event {
                    ChainEvent::HopStarted(_) => None,
                    ChainEvent::HostKeyPrompt { hop, prompt } => Some(LinkEvent::HostKey {
                        host_id: Some(hop.host_id),
                        prompt,
                    }),
                    ChainEvent::ProxyConfirmation { hop, command } => Some(LinkEvent::Proxy {
                        host_id: hop.host_id,
                        label: hop.to_string(),
                        command,
                    }),
                    ChainEvent::HopReady(hop) => Some(LinkEvent::HopReady(hop.host_id)),
                    ChainEvent::Connected { .. } => Some(LinkEvent::Connected),
                    ChainEvent::X11 { hop, status } => Some(LinkEvent::X11 {
                        host_id: Some(hop.host_id),
                        status,
                    }),
                    ChainEvent::Data(bytes) => Some(LinkEvent::Data(bytes)),
                    ChainEvent::Exited { code } => Some(LinkEvent::Exited { code }),
                    ChainEvent::Failed(failure) => Some(LinkEvent::FailedChain(failure)),
                })
                .collect(),
        }
    }

    /// Answers a trust question.
    ///
    /// A chain needs the hop id. `true` means the answer was queued, not that the hop
    /// finished using it.
    #[must_use]
    pub fn answer_host_key(&mut self, host_id: Option<HostId>, accept: bool) -> bool {
        match self {
            Self::Remote(session) => session.answer_host_key(accept),
            Self::Chain(session) => {
                host_id.is_some_and(|host_id| session.answer_host_key(host_id, accept))
            }
        }
    }

    /// Confirms or refuses one hop's ProxyCommand. A direct session is not waiting.
    #[must_use]
    pub fn confirm_proxy(&mut self, host_id: HostId, allow: bool) -> bool {
        match self {
            Self::Remote(_) => false,
            Self::Chain(session) => session.confirm_proxy(host_id, allow),
        }
    }

    /// The browsable side, once.
    pub fn take_file_link(&mut self) -> Option<FileLink> {
        match self {
            Self::Remote(session) => session.take_file_link(),
            Self::Chain(session) => session.take_file_link(),
        }
    }

    /// The transfer side, once.
    pub fn take_transfer_link(&mut self) -> Option<TransferLink> {
        match self {
            Self::Remote(session) => session.take_transfer_link(),
            Self::Chain(session) => session.take_transfer_link(),
        }
    }

    /// The forwarding side, once.
    pub fn take_forward_link(&mut self) -> Option<shellune_ssh::ForwardLink> {
        match self {
            Self::Remote(session) => session.take_forward_link(),
            Self::Chain(session) => session.take_forward_link(),
        }
    }
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

    #[test]
    fn a_chain_authentication_failure_names_the_hop_the_session_failure_drops() {
        let bastion = HostConfig::new("bastion", "10.0.0.1", 22, "jump").expect("a hop");
        let failure = ChainFailure::Hop {
            hop: Box::new(shellune_ssh::HopLocation {
                host_id: bastion.id(),
                index: 0,
                name: bastion.name().to_owned(),
                address: bastion.address().to_owned(),
                port: bastion.port().get(),
                user: bastion.user().to_owned(),
            }),
            error: Box::new(ConnectError::AuthRejected),
        };
        let shown = RemoteFailure::from_chain(&failure);
        assert!(
            shown.reason().contains("bastion"),
            "the panel names the hop: {}",
            shown.reason()
        );
        assert!(
            shown.reason().contains("hop"),
            "the panel says which hop failed: {}",
            shown.reason()
        );
        let recorded = failure.session_failure();
        assert!(
            matches!(
                recorded,
                shellune_core::SessionFailure::AuthenticationFailed
            ),
            "the session record drops the hop, got {recorded:?}"
        );
        assert!(
            !recorded.to_string().contains("bastion"),
            "session_failure does not name the hop: {recorded}"
        );
    }

    #[test]
    fn a_host_with_both_a_jump_and_a_proxy_command_is_refused_before_dial() {
        let bastion = HostConfig::new("bastion", "10.0.0.1", 22, "jump").expect("a hop");
        let mut web = HostConfig::new("web", "10.0.0.2", 22, "deploy").expect("a target");
        web.set_jumps([bastion.id()]);
        web.set_proxy_command(Some("nc %h %p"));
        let draft = ChainDraft::prepare(&[bastion, web.clone()], &web);
        let fault = draft.fault().expect("both routes are refused");
        assert!(fault.contains("Nothing was connected"), "{fault}");
        assert!(fault.contains("no command was run"), "{fault}");
        assert!(draft.order().is_empty());
        assert!(draft.into_request(PtySize::new(80, 24)).is_none());
    }
}
