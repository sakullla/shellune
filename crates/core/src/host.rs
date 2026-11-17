//! Validated SSH host configuration (R3, R10).
//!
//! A [`HostConfig`] can only exist with a non-empty name, address and user plus a
//! port in `1..=65535`, whether it was built in code, edited later or read back from
//! storage. Saving a host never implies a connection was made.
//!
//! A host may name an ordered jump chain and a ProxyCommand. Both are configuration:
//! the chain is a list of saved host ids, and the command is text. Neither is
//! dialed or executed by this module. A password still has nowhere to live.

use std::collections::HashMap;

use crate::auth::AuthMethodKind;
use serde::{Deserialize, Serialize};

use crate::connection::{ConnectionDefaults, ConnectionPolicy, HostConnection};
use crate::group::GroupPath;
use crate::ids::HostId;
use crate::session::SessionCapabilities;
use crate::task::TransferProtocol;

/// Port used when the user does not specify one.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// How much an SSH session may forward X11 to the local display (R11).
///
/// The three values are the ones OpenSSH names on its command line: `-X` is
/// [`Self::Restricted`], `-Y` is [`Self::Trusted`], and no flag is [`Self::Off`].
/// They are not three spellings of "on": the trusted mode lets the server reach the
/// local display without the X11 SECURITY extension restrictions, so a remote program
/// can read the keystrokes of other windows of that display. That is why the default
/// is off and why a trusted connection asks again every time.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum X11Forwarding {
    /// No forwarding. No request is sent and no remote `DISPLAY` is set.
    #[default]
    Off,
    /// Forwarding limited by the X11 SECURITY extension, like `ssh -X`.
    Restricted,
    /// Forwarding without those limits, like `ssh -Y`.
    Trusted,
}

impl X11Forwarding {
    /// Every mode, in the order a chooser shows them.
    pub const ALL: [Self; 3] = [Self::Off, Self::Restricted, Self::Trusted];

    /// Whether this mode asks the server for a channel at all.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        self != Self::Off
    }

    /// Whether this mode needs the per-connection risk confirmation.
    ///
    /// Only the trusted mode does: it is the one that lifts the SECURITY extension
    /// limits on what the remote programs may do with the local display.
    #[must_use]
    pub fn needs_risk_confirmation(self) -> bool {
        self == Self::Trusted
    }
}

impl std::fmt::Display for X11Forwarding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Restricted => "restricted",
            Self::Trusted => "trusted",
        })
    }
}

/// A TCP port that is known to be usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Port(u16);

impl Port {
    /// The SSH default port.
    pub const DEFAULT: Self = Self(DEFAULT_SSH_PORT);

    /// Validates a user supplied port value.
    ///
    /// # Errors
    ///
    /// Returns [`HostValidationError::PortOutOfRange`] for `0` and for anything
    /// above `65535`.
    pub fn new(raw: u32) -> Result<Self, HostValidationError> {
        match u16::try_from(raw) {
            Ok(value) if value > 0 => Ok(Self(value)),
            _ => Err(HostValidationError::PortOutOfRange { value: raw }),
        }
    }

    /// Returns the numeric port.
    #[must_use]
    pub fn get(self) -> u16 {
        self.0
    }
}

impl Default for Port {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for Port {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

/// Why a host configuration was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostValidationError {
    /// The display name is empty or only whitespace.
    #[error("host name cannot be empty")]
    EmptyName,
    /// The address is empty or only whitespace.
    #[error("host address cannot be empty")]
    EmptyAddress,
    /// The address contains whitespace and cannot be resolved.
    #[error("host address cannot contain whitespace")]
    AddressContainsWhitespace,
    /// The user name is empty or only whitespace.
    #[error("user name cannot be empty")]
    EmptyUser,
    /// The port falls outside `1..=65535`.
    #[error("port {value} is outside the valid range 1-65535")]
    PortOutOfRange {
        /// The rejected value, kept as an integer so the message is exact.
        value: u32,
    },
}

/// A saved SSH host.
///
/// Fields are private so every mutation goes through a validating constructor or
/// setter; [`serde::Deserialize`] re-uses the same checks via `try_from`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HostConfigWire")]
pub struct HostConfig {
    id: HostId,
    name: String,
    address: String,
    port: Port,
    user: String,
    group: Option<String>,
    /// The hierarchy path of the group this host is in (R18), when the user chose one.
    ///
    /// A record carries at most one of this and [`Self::group`]: `group` is the single
    /// label an earlier build wrote, `group_path` is the hierarchy this build assigns.
    /// Keeping them apart is what lets the label stay byte-identical on disk and read as
    /// a path of one segment even when the label itself contains the separator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_path: Option<GroupPath>,
    /// The *path* of a private key file, never its contents.
    ///
    /// An imported SSH configuration sets `IdentityFile`; keeping the path lets a
    /// later connection use the key the user already chose, while the record still
    /// has nowhere to store key material or a passphrase. A host without one omits
    /// the field entirely, so a record saved by a build that had no such field
    /// stays byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    identity_file: Option<String>,
    /// How the next connection signs in, when the user chose it in the host form.
    ///
    /// A kind, never a secret. Absent for a host saved before the choice existed, which
    /// keeps asking at connect time as it always did.
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<AuthMethodKind>,
    /// Whether the password or key passphrase of this host is kept in the system
    /// credential store.
    ///
    /// Only the fact is recorded here; the secret itself never enters this record or
    /// its file. Knowing it without asking the credential store means a host that has
    /// nothing saved never makes the system store prompt.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    remember_secret: bool,
    /// A command typed into the remote shell once each connection is live.
    #[serde(skip_serializing_if = "Option::is_none")]
    startup_command: Option<String>,
    /// Ordered jump hosts visited before this one.
    ///
    /// Each entry is the id of a saved host, not a copy of that host and not a
    /// secret. Absent on records written before jump chains existed, which is a
    /// direct connection. The list is not dialed here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    jumps: Vec<HostId>,
    /// ProxyCommand text for this host.
    ///
    /// The text is what a later, confirmed connection would run. Persisting or
    /// restoring the host does not run it, and the field is not a place to put a
    /// password: there is still no secret field beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proxy_command: Option<String>,
    /// Which file-copy protocol this host's transfers use by default (R10).
    ///
    /// Absent means the SFTP default a record written before the choice existed meant,
    /// so an older file stays byte-identical. The choice is a preference, not a
    /// fallback: a transfer started over SCP reports an SCP failure and never silently
    /// becomes an SFTP one, and the reverse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transfer_protocol: Option<TransferProtocol>,
    /// How an SSH session to this host may forward X11, when the user chose (R11).
    ///
    /// Absent means [`X11Forwarding::Off`], which is also what a record written before
    /// the choice existed means, so an older file stays byte-identical and no session
    /// starts asking a server for a display channel without the user saying so. A
    /// single connection may choose another mode from this host's default; that choice
    /// belongs to the attempt and does not change this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    x11: Option<X11Forwarding>,
    /// What this host chose for its own connection behaviour (R20).
    ///
    /// Absent means every field inherits the global default, which is also what a
    /// record written before the choice existed means, so an older file stays
    /// byte-identical. The record is kept as one value because the three fields are
    /// read from and written to the same form: a host answers "inherit", "keepalive
    /// off" or "these numbers" as one decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<HostConnection>,
}

/// The stored shape of a host.
///
/// Unknown fields are rejected rather than ignored: a file that carries a
/// password, a passphrase or any other field this build does not write is not a
/// host record this build wrote, and loading it would silently accept a credential
/// into a model that promises to hold none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostConfigWire {
    id: HostId,
    name: String,
    address: String,
    port: Port,
    user: String,
    group: Option<String>,
    /// Absent in files written before groups could nest.
    #[serde(default)]
    group_path: Option<GroupPath>,
    /// Absent in files written before an imported key path was stored.
    #[serde(default)]
    identity_file: Option<String>,
    /// Absent in files written before the sign-in choice was stored.
    #[serde(default)]
    auth: Option<AuthMethodKind>,
    #[serde(default)]
    remember_secret: bool,
    #[serde(default)]
    startup_command: Option<String>,
    /// Absent in files written before jump chains existed.
    #[serde(default)]
    jumps: Vec<HostId>,
    /// Absent in files written before ProxyCommand was stored.
    #[serde(default)]
    proxy_command: Option<String>,
    /// Absent in files written before the default copy protocol was stored.
    #[serde(default)]
    transfer_protocol: Option<TransferProtocol>,
    /// Absent in files written before an X11 mode was stored.
    #[serde(default)]
    x11: Option<X11Forwarding>,
    /// Absent in files written before connection overrides were stored.
    #[serde(default)]
    connection: Option<HostConnection>,
}

impl TryFrom<HostConfigWire> for HostConfig {
    type Error = HostValidationError;

    fn try_from(wire: HostConfigWire) -> Result<Self, Self::Error> {
        let name = validate_name(wire.name)?;
        let address = validate_address(wire.address)?;
        let user = validate_user(wire.user)?;
        // The port deserializes as a bare integer, so the range check has to be
        // repeated here; otherwise a stored `port: 0` would become a connectable host.
        let port = Port::new(u32::from(wire.port.get()))?;
        // One place, never two: a stored hierarchy wins, and the label is the
        // one-segment path an earlier build wrote. Reading does not guess a
        // hierarchy out of a label that contains the separator.
        let (group, group_path) = match wire.group_path {
            Some(path) => (None, Some(path)),
            None => (
                wire.group
                    .map(|group| group.trim().to_owned())
                    .filter(|group| !group.is_empty()),
                None,
            ),
        };
        Ok(Self {
            id: wire.id,
            name,
            address,
            port,
            user,
            group_path,
            group,
            identity_file: normalize_identity_file(wire.identity_file),
            auth: wire.auth,
            remember_secret: wire.remember_secret,
            startup_command: normalize_command(wire.startup_command),
            jumps: wire.jumps,
            proxy_command: normalize_proxy_command(wire.proxy_command),
            transfer_protocol: wire
                .transfer_protocol
                .filter(|protocol| *protocol != TransferProtocol::Sftp),
            // The off default is stored as no choice at all, so a record that never
            // asked for X11 keeps the shape an older build wrote.
            x11: wire.x11.filter(|mode| *mode != X11Forwarding::Off),
            // A record that states nothing is inheritance, and inheritance is stored
            // as no field at all, so an untouched host keeps the shape it had.
            connection: wire.connection.filter(|connection| !connection.is_empty()),
        })
    }
}

fn validate_name(name: String) -> Result<String, HostValidationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(HostValidationError::EmptyName);
    }
    Ok(trimmed.to_owned())
}

fn validate_address(address: String) -> Result<String, HostValidationError> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err(HostValidationError::EmptyAddress);
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(HostValidationError::AddressContainsWhitespace);
    }
    Ok(trimmed.to_owned())
}

fn validate_user(user: String) -> Result<String, HostValidationError> {
    let trimmed = user.trim();
    if trimmed.is_empty() {
        return Err(HostValidationError::EmptyUser);
    }
    Ok(trimmed.to_owned())
}

impl HostConfig {
    /// Creates a host configuration.
    ///
    /// # Errors
    ///
    /// Returns [`HostValidationError`] when a required field is empty, the address
    /// contains whitespace, or the port is invalid.
    pub fn new(
        name: impl Into<String>,
        address: impl Into<String>,
        port: u16,
        user: impl Into<String>,
    ) -> Result<Self, HostValidationError> {
        Ok(Self {
            id: HostId::new(),
            name: validate_name(name.into())?,
            address: validate_address(address.into())?,
            port: Port::new(u32::from(port))?,
            user: validate_user(user.into())?,
            group: None,
            group_path: None,
            identity_file: None,
            auth: None,
            remember_secret: false,
            startup_command: None,
            jumps: Vec::new(),
            proxy_command: None,
            transfer_protocol: None,
            x11: None,
            connection: None,
        })
    }

    /// Stable identifier used by sessions and storage.
    #[must_use]
    pub fn id(&self) -> HostId {
        self.id
    }

    /// User visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Host name or IP address.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// TCP port.
    #[must_use]
    pub fn port(&self) -> Port {
        self.port
    }

    /// Login user.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The single grouping label an earlier build wrote, when the record has one.
    ///
    /// A host in a hierarchy returns `None` here and its path from
    /// [`Self::group_path`]; use [`Self::group_text`] where one string is needed.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The group this host is in, as a path, when it is grouped at all.
    ///
    /// A record written before groups could nest answers with its label as a path of
    /// one segment, which is what that label meant. A label that has no path form (it
    /// carries a control character) answers `None` and stays visible as a label.
    #[must_use]
    pub fn group_path(&self) -> Option<GroupPath> {
        match (&self.group_path, &self.group) {
            (Some(path), _) => Some(path.clone()),
            (None, Some(label)) => GroupPath::from_label(label).ok(),
            (None, None) => None,
        }
    }

    /// The legacy label this host still carries although it has no path form.
    ///
    /// A label with a control character or one longer than a group name is not a
    /// group this build can draw, so [`Self::group_path`] answers `None` for it. It
    /// stays a label on purpose — rewriting it would lose what the user saved — which
    /// is why the host book lists these records instead of counting them as
    /// ungrouped and saying nothing.
    #[must_use]
    pub fn group_label_without_path(&self) -> Option<&str> {
        match (&self.group_path, &self.group) {
            (None, Some(label)) if GroupPath::from_label(label).is_err() => Some(label.as_str()),
            _ => None,
        }
    }

    /// The group as one string, for search and the flat label view.
    ///
    /// A hierarchy is written in its canonical form, so searching `Prod/Web` finds the
    /// hosts inside it.
    #[must_use]
    pub fn group_text(&self) -> Option<String> {
        match (&self.group_path, &self.group) {
            (Some(path), _) => Some(path.to_canonical()),
            (None, Some(label)) => Some(label.clone()),
            (None, None) => None,
        }
    }

    /// The private key file the next connection should offer, when one was
    /// recorded (an imported `IdentityFile`).
    ///
    /// Only the path is kept: the key material and any passphrase stay out of the
    /// record, of the document and of the logs.
    #[must_use]
    pub fn identity_file(&self) -> Option<&str> {
        self.identity_file.as_deref()
    }

    /// How the next connection signs in, when the user chose it.
    #[must_use]
    pub fn auth(&self) -> Option<AuthMethodKind> {
        self.auth
    }

    /// Whether this host's password or passphrase is kept in the system credential
    /// store.
    #[must_use]
    pub fn remembers_secret(&self) -> bool {
        self.remember_secret
    }

    /// The command typed into the remote shell once a connection is live.
    #[must_use]
    pub fn startup_command(&self) -> Option<&str> {
        self.startup_command.as_deref()
    }

    /// Jump hosts visited before this one, in order.
    ///
    /// Empty means a direct connection. The ids are references; a missing or
    /// cyclic reference is reported by [`jump_faults`] and is not dialed here.
    #[must_use]
    pub fn jumps(&self) -> &[HostId] {
        &self.jumps
    }

    /// ProxyCommand text, when the user configured one.
    ///
    /// This is the command that would need an explicit confirmation before it
    /// could run. Reading the host does not run it.
    #[must_use]
    pub fn proxy_command(&self) -> Option<&str> {
        self.proxy_command.as_deref()
    }

    /// The file-copy protocol this host's transfers start with (R10).
    ///
    /// SFTP unless the user chose otherwise, so a host saved before the choice existed
    /// answers the same thing it always did. A single transfer may still choose the
    /// other protocol, which is a choice for that task and not a change to this host.
    #[must_use]
    pub fn transfer_protocol(&self) -> TransferProtocol {
        self.transfer_protocol.unwrap_or_default()
    }

    /// How a session to this host forwards X11 unless the connection form says
    /// otherwise (R11).
    ///
    /// [`X11Forwarding::Off`] unless the user chose a mode, so a host saved before the
    /// choice existed never asks a server for a display channel. The value is a
    /// default for new connections; it does not change a session that is already live.
    #[must_use]
    pub fn x11(&self) -> X11Forwarding {
        self.x11.unwrap_or_default()
    }

    /// What this host chose for its own keepalive and connect timeout (R20).
    ///
    /// `None` means the host states nothing and every value inherits the global
    /// default. A value states at least one field: an interval of `0` is keepalive
    /// switched off for this host, and an absent field inside the record is that one
    /// field inheriting.
    #[must_use]
    pub fn connection(&self) -> Option<HostConnection> {
        self.connection
    }

    /// The policy this host is dialled with, given the global defaults (R20).
    ///
    /// This is the only place the two are combined, so a direct session and a jump
    /// chain cannot resolve the same host differently.
    #[must_use]
    pub fn connection_policy(&self, defaults: ConnectionDefaults) -> ConnectionPolicy {
        defaults.policy_for(self.connection)
    }

    /// `user@address:port`, the form used in confirmation prompts.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}@{}:{}", self.user, self.address, self.port)
    }

    /// Renames the host.
    ///
    /// # Errors
    ///
    /// Returns [`HostValidationError::EmptyName`] when the name is blank.
    pub fn set_name(&mut self, name: impl Into<String>) -> Result<(), HostValidationError> {
        self.name = validate_name(name.into())?;
        Ok(())
    }

    /// Changes the address used by the next connection attempt.
    ///
    /// # Errors
    ///
    /// Returns [`HostValidationError`] for a blank or whitespace containing address.
    pub fn set_address(&mut self, address: impl Into<String>) -> Result<(), HostValidationError> {
        self.address = validate_address(address.into())?;
        Ok(())
    }

    /// Changes the port used by the next connection attempt.
    ///
    /// # Errors
    ///
    /// Returns [`HostValidationError::PortOutOfRange`] for `0` or values above `65535`.
    pub fn set_port(&mut self, port: u32) -> Result<(), HostValidationError> {
        self.port = Port::new(port)?;
        Ok(())
    }

    /// Changes the login user.
    ///
    /// # Errors
    ///
    /// Returns [`HostValidationError::EmptyUser`] when the user is blank.
    pub fn set_user(&mut self, user: impl Into<String>) -> Result<(), HostValidationError> {
        self.user = validate_user(user.into())?;
        Ok(())
    }

    /// Assigns or clears the single grouping label.
    ///
    /// A label that is actually set replaces any hierarchy this host was in: a host
    /// belongs to one place, and two fields that could disagree would make the group
    /// counts wrong. A blank label leaves the hierarchy alone, because a blank is what
    /// an editor submits for a host whose grouping this field cannot express - a nested
    /// group, which has a path and no label. Reading that blank as "leave every group"
    /// would ungroup such a host on every save of any other field of it. Moving a host
    /// out of every group is the job of [`Self::set_group_path`] with `None`, which
    /// clears the label as well.
    pub fn set_group(&mut self, group: Option<impl Into<String>>) {
        let group = group
            .map(Into::into)
            .map(|group: String| group.trim().to_owned())
            .filter(|group| !group.is_empty());
        // Only an actual label replaces the hierarchy; a blank one leaves the path
        // untouched, so editing an unrelated field cannot silently ungroup the host.
        if group.is_some() {
            self.group_path = None;
        }
        self.group = group;
    }

    /// Moves this host into a group path, or out of every group.
    ///
    /// The path was validated when it was built, so this cannot fail. The label is
    /// always cleared, including when the destination is `None`: a host moved out of
    /// its group has to leave it for real, and keeping the old label would leave it
    /// filed under the group the user just removed it from.
    pub fn set_group_path(&mut self, path: Option<GroupPath>) {
        self.group = None;
        self.group_path = path;
    }

    /// Assigns or clears the private key path.
    ///
    /// A blank path clears the field, so an editor that submits an empty text box
    /// removes the reference instead of storing nothing that looks like a key.
    pub fn set_identity_file(&mut self, path: Option<impl Into<String>>) {
        self.identity_file = normalize_identity_file(path.map(Into::into));
    }

    /// Chooses how the next connection signs in, or leaves it to be asked.
    pub fn set_auth(&mut self, auth: Option<AuthMethodKind>) {
        self.auth = auth;
        // A method that reads no secret has nothing to remember.
        if !auth.is_some_and(|kind| kind.needs_password() || kind.needs_passphrase()) {
            self.remember_secret = false;
        }
    }

    /// Records whether the secret of this host is in the system credential store.
    ///
    /// Only a method that reads a secret can remember one.
    pub fn set_remember_secret(&mut self, remember: bool) {
        self.remember_secret = remember
            && self
                .auth
                .is_some_and(|kind| kind.needs_password() || kind.needs_passphrase());
    }

    /// Sets or clears the command run after connecting. A blank command clears it.
    pub fn set_startup_command(&mut self, command: Option<impl Into<String>>) {
        self.startup_command = normalize_command(command.map(Into::into));
    }

    /// Replaces the jump chain. Order is kept, including entries that do not
    /// resolve yet, so an edit can be saved and repaired instead of discarded.
    pub fn set_jumps(&mut self, jumps: impl IntoIterator<Item = HostId>) {
        self.jumps = jumps.into_iter().collect();
    }

    /// Sets or clears the ProxyCommand. A blank command clears it and nothing is run.
    pub fn set_proxy_command(&mut self, command: Option<impl Into<String>>) {
        self.proxy_command = normalize_proxy_command(command.map(Into::into));
    }

    /// Chooses the protocol this host's transfers start with.
    ///
    /// The SFTP default is stored as "nothing", so choosing it again leaves a record
    /// written by an older build byte-identical instead of growing a field that says
    /// the same thing.
    pub fn set_transfer_protocol(&mut self, protocol: TransferProtocol) {
        self.transfer_protocol = (protocol != TransferProtocol::Sftp).then_some(protocol);
    }

    /// Chooses how new sessions to this host forward X11.
    ///
    /// The off default is stored as "nothing", so choosing it again leaves a record
    /// written by an older build byte-identical instead of growing a field that says
    /// the same thing. A live session is not changed by this: the mode belongs to the
    /// connection it was dialled with.
    pub fn set_x11(&mut self, mode: X11Forwarding) {
        self.x11 = (mode != X11Forwarding::Off).then_some(mode);
    }

    /// Records or clears this host's own connection values (R20).
    ///
    /// A record that states nothing clears the field, so a host whose last override was
    /// removed returns to inheriting and its file shrinks back to what a build without
    /// the choice wrote. Choosing a value that would mean "inherit" is not stored,
    /// because a second spelling of the same thing would make an edit look changed.
    pub fn set_connection(&mut self, connection: Option<HostConnection>) {
        self.connection = connection.filter(|connection| !connection.is_empty());
    }
}

/// Trims a startup command, joins its lines, and turns a blank one into `None`.
///
/// The command is typed into the shell as one line followed by Enter, so a line break
/// inside it would run half of it early; lines are joined with "; " instead.
fn normalize_command(command: Option<String>) -> Option<String> {
    command
        .map(|command| {
            command
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|command| !command.is_empty())
}

/// Trims a ProxyCommand and turns a blank one into `None`.
///
/// The text is stored unchanged apart from surrounding whitespace. Newlines stay
/// in the value so a round trip does not invent a different command, and nothing
/// in this path invokes a shell.
fn normalize_proxy_command(command: Option<String>) -> Option<String> {
    command
        .map(|command| command.trim().to_owned())
        .filter(|command| !command.is_empty())
}

/// Trims a key path and turns a blank one into `None`.
fn normalize_identity_file(path: Option<String>) -> Option<String> {
    path.map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
}

// ---------------------------------------------------------------------------
// Protocol identity and the configuration each protocol keeps (R2, R9, R18)
// ---------------------------------------------------------------------------
//
// A saved connection is one of four kinds, and every kind keeps its own
// configuration instead of sharing one record full of optional SSH fields. A record
// written before this choice existed carries no protocol tag at all, and that is
// exactly what an SSH connection means, so an older file keeps its meaning.

/// What a saved connection speaks.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    /// Secure Shell: a character session with files, batch, forwarding and X11.
    #[default]
    Ssh,
    /// Remote Desktop Protocol: a desktop session.
    Rdp,
    /// RFB/VNC: a desktop session.
    Vnc,
    /// A serial device: a character session over a local port (R21).
    Serial,
}

impl ProtocolKind {
    /// Every protocol, in the order a chooser shows them.
    pub const ALL: [Self; 4] = [Self::Ssh, Self::Rdp, Self::Vnc, Self::Serial];

    /// What the tag says in a stored record.
    ///
    /// The same string the serde representation uses, so a file, a log line and a
    /// test all name the protocol identically.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Rdp => "rdp",
            Self::Vnc => "vnc",
            Self::Serial => "serial",
        }
    }

    /// Short label used beside a connection name.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ssh => "SSH",
            Self::Rdp => "RDP",
            Self::Vnc => "VNC",
            Self::Serial => "Serial",
        }
    }

    /// The port this protocol uses when the user does not choose one.
    ///
    /// A serial port has no network port, which is why the answer is optional
    /// rather than a number every protocol has to invent.
    #[must_use]
    pub fn default_port(self) -> Option<Port> {
        match self {
            Self::Ssh => Some(Port(DEFAULT_SSH_PORT)),
            Self::Rdp => Some(Port(DEFAULT_RDP_PORT)),
            Self::Vnc => Some(Port(DEFAULT_VNC_PORT)),
            Self::Serial => None,
        }
    }

    /// Whether this protocol is the one that carries files, batch, forwarding and X11.
    ///
    /// Named for what the code decides with it: only an SSH connection may be a
    /// batch target, a forwarding host or an X11 client.
    #[must_use]
    pub fn is_ssh(self) -> bool {
        matches!(self, Self::Ssh)
    }

    /// Whether the connection is reached over the network.
    #[must_use]
    pub fn is_network(self) -> bool {
        matches!(self, Self::Ssh | Self::Rdp | Self::Vnc)
    }

    /// Whether the session is drawn as a character grid.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ssh | Self::Serial)
    }

    /// Whether the session is drawn as a framebuffer.
    #[must_use]
    pub fn is_desktop(self) -> bool {
        matches!(self, Self::Rdp | Self::Vnc)
    }

    /// The protocol a stored tag names, or `None` for a word this build does not know.
    ///
    /// A caller that meets an unknown word keeps it as an unknown protocol rather
    /// than turning it into a connection it could dial.
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ssh" => Some(Self::Ssh),
            "rdp" => Some(Self::Rdp),
            "vnc" => Some(Self::Vnc),
            "serial" => Some(Self::Serial),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProtocolKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.label(), formatter)
    }
}

/// Port an RDP connection uses when the user does not choose one.
pub const DEFAULT_RDP_PORT: u16 = 3389;

/// Port a VNC connection uses when the user does not choose one.
pub const DEFAULT_VNC_PORT: u16 = 5900;

/// Line speed a serial connection starts with when the user does not choose one.
pub const DEFAULT_SERIAL_BAUD: u32 = 115_200;

/// Slowest line speed this build accepts.
pub const MIN_SERIAL_BAUD: u32 = 50;

/// Fastest line speed this build accepts.
pub const MAX_SERIAL_BAUD: u32 = 4_000_000;

/// Data bits a serial frame can carry.
pub const MIN_SERIAL_DATA_BITS: u8 = 5;

/// Widest serial frame this build accepts.
pub const MAX_SERIAL_DATA_BITS: u8 = 8;

/// How a serial frame carries parity.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SerialParity {
    /// No parity bit.
    #[default]
    None,
    /// Even parity.
    Even,
    /// Odd parity.
    Odd,
    /// Mark (always one) parity.
    Mark,
    /// Space (always zero) parity.
    Space,
}

impl SerialParity {
    /// Every parity, in the order a chooser shows them.
    pub const ALL: [Self; 5] = [Self::None, Self::Even, Self::Odd, Self::Mark, Self::Space];

    /// Short label used in the form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Even => "Even",
            Self::Odd => "Odd",
            Self::Mark => "Mark",
            Self::Space => "Space",
        }
    }
}

/// How many stop bits a serial frame ends with.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SerialStopBits {
    /// One stop bit.
    #[default]
    One,
    /// Two stop bits.
    Two,
}

impl SerialStopBits {
    /// Every choice, in the order a chooser shows them.
    pub const ALL: [Self; 2] = [Self::One, Self::Two];

    /// Short label used in the form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::One => "1",
            Self::Two => "2",
        }
    }
}

/// How the serial port paces the bytes it carries.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SerialFlow {
    /// No pacing.
    #[default]
    None,
    /// XON/XOFF characters.
    Software,
    /// RTS/CTS lines.
    Hardware,
}

impl SerialFlow {
    /// Every choice, in the order a chooser shows them.
    pub const ALL: [Self; 3] = [Self::None, Self::Software, Self::Hardware];

    /// Short label used in the form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Software => "XON/XOFF",
            Self::Hardware => "RTS/CTS",
        }
    }
}

/// Why a non-SSH connection configuration was rejected.
///
/// Kept apart from `HostValidationError` so that the SSH rules a caller already
/// matches on do not grow variants that mean something else.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionValidationError {
    /// The display name is empty or only whitespace.
    #[error("connection name cannot be empty")]
    EmptyName,
    /// The address is empty or only whitespace.
    #[error("host address cannot be empty")]
    EmptyAddress,
    /// The address contains whitespace and cannot be resolved.
    #[error("host address cannot contain whitespace")]
    AddressContainsWhitespace,
    /// The port falls outside `1..=65535`.
    #[error("port {value} is outside the valid range 1-65535")]
    PortOutOfRange {
        /// The rejected value, kept as an integer so the message is exact.
        value: u32,
    },
    /// The user name is present but contains whitespace.
    #[error("user name cannot contain whitespace")]
    UserContainsWhitespace,
    /// The device name is empty or only whitespace.
    #[error("device name cannot be empty")]
    EmptyDevice,
    /// The device name contains a control character.
    #[error("device name cannot contain control characters")]
    DeviceContainsControlCharacter,
    /// The folder shared with a remote desktop contains a control character.
    #[error("folder path cannot contain control characters")]
    DriveFolderContainsControlCharacter,
    /// The line speed is outside the range this build accepts.
    #[error("baud rate {value} is outside the range {MIN_SERIAL_BAUD}-{MAX_SERIAL_BAUD}")]
    BaudOutOfRange {
        /// The rejected value, kept as an integer so the message is exact.
        value: u32,
    },
    /// The frame width is not one a serial port can use.
    #[error("data bits {value} is outside the range {MIN_SERIAL_DATA_BITS}-{MAX_SERIAL_DATA_BITS}")]
    DataBitsOutOfRange {
        /// The rejected value, kept as a number so the message is exact.
        value: u8,
    },
}

/// How much a VNC connection insists on encryption (R18, R20 in the wider sense).
///
/// This is a policy the connection will follow, not a claim about the server: the
/// session reports the security type it actually negotiated, and an
/// `EncryptedOnly` connection that cannot reach an encrypted type fails instead of
/// silently using a weaker one.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum VncSecurity {
    /// Accept whatever security type the server offers and report what it was.
    #[default]
    Automatic,
    /// Refuse a connection whose negotiated security type is not encrypted.
    EncryptedOnly,
}

impl VncSecurity {
    /// Every choice, in the order a chooser shows them.
    pub const ALL: [Self; 2] = [Self::Automatic, Self::EncryptedOnly];

    /// Short label used in the form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Server default",
            Self::EncryptedOnly => "Encrypted only",
        }
    }
}

/// What a RDP connection does with a server certificate it has not been told to trust.
///
/// The fingerprint is still shown before a decision is taken; this policy decides
/// whether the user is asked at all or the attempt ends. It never means "accept
/// anything silently".
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RdpCertificatePolicy {
    /// Show the certificate and let the user decide, every time.
    #[default]
    Ask,
    /// End the attempt when the certificate is not trusted, without asking.
    Reject,
}

impl RdpCertificatePolicy {
    /// Every choice, in the order a chooser shows them.
    pub const ALL: [Self; 2] = [Self::Ask, Self::Reject];

    /// Short label used in the form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask every time",
            Self::Reject => "Reject untrusted",
        }
    }
}

/// Whether a policy is the default one, so a file can leave it out.
fn certificate_is_default(policy: &RdpCertificatePolicy) -> bool {
    *policy == RdpCertificatePolicy::Ask
}

/// Whether a security choice is the default one, so a file can leave it out.
fn security_is_default(security: &VncSecurity) -> bool {
    *security == VncSecurity::Automatic
}

/// The clipboard of a desktop connection is shared unless the user turned it off.
const fn clipboard_default() -> bool {
    true
}

/// Whether the clipboard choice is the default one, so a file can leave it out.
///
/// Only a connection that turned the clipboard off carries the key, which keeps
/// every other record in the shape an older build reads.
fn clipboard_is_default(clipboard: &bool) -> bool {
    *clipboard
}

/// Trims a folder path, turns a blank one into `None` and refuses a control
/// character, which no folder a person can choose contains.
fn connection_drive_folder(
    folder: Option<String>,
) -> Result<Option<String>, ConnectionValidationError> {
    let folder = folder
        .map(|folder| folder.trim().to_owned())
        .filter(|folder| !folder.is_empty());
    if folder
        .as_deref()
        .is_some_and(|folder| folder.chars().any(char::is_control))
    {
        return Err(ConnectionValidationError::DriveFolderContainsControlCharacter);
    }
    Ok(folder)
}

/// A saved RDP connection (R18).
///
/// The record holds what the next connection needs to find and sign in to a
/// desktop: address, port, an optional user and an optional domain. A password has
/// nowhere to live here; `remember_secret` only records that the system credential
/// store may hold one for this identifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RdpConfigWire")]
pub struct RdpConfig {
    id: HostId,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_path: Option<GroupPath>,
    address: String,
    port: Port,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// The Windows domain or realm the user belongs to, when the server needs it.
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
    /// What the connection does with a certificate it has not been told to trust.
    #[serde(default, skip_serializing_if = "certificate_is_default")]
    certificate: RdpCertificatePolicy,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    remember_secret: bool,
    /// Whether text and pictures copied on either side can be pasted on the other.
    #[serde(
        default = "clipboard_default",
        skip_serializing_if = "clipboard_is_default"
    )]
    clipboard: bool,
    /// A folder on this machine the remote desktop sees as a drive, when one was
    /// chosen.
    ///
    /// The path only means something on this machine: a portable document keeps it
    /// in the section of the platform that chose it, never in the shared record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drive_folder: Option<String>,
}

/// The stored shape of an RDP connection.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RdpConfigWire {
    id: HostId,
    name: String,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    group_path: Option<GroupPath>,
    address: String,
    port: Port,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    certificate: RdpCertificatePolicy,
    #[serde(default)]
    remember_secret: bool,
    #[serde(default = "clipboard_default")]
    clipboard: bool,
    #[serde(default)]
    drive_folder: Option<String>,
}

impl TryFrom<RdpConfigWire> for RdpConfig {
    type Error = ConnectionValidationError;

    fn try_from(wire: RdpConfigWire) -> Result<Self, Self::Error> {
        let (group, group_path) = split_group(wire.group, wire.group_path);
        Ok(Self {
            id: wire.id,
            name: connection_name(wire.name)?,
            group,
            group_path,
            address: connection_address(wire.address)?,
            // The range check is repeated here: a stored `port: 0` would otherwise
            // become a connection that dials nothing.
            port: connection_port(u32::from(wire.port.get()))?,
            user: connection_user(wire.user)?,
            domain: normalize_optional_text(wire.domain),
            certificate: wire.certificate,
            remember_secret: wire.remember_secret,
            clipboard: wire.clipboard,
            drive_folder: connection_drive_folder(wire.drive_folder)?,
        })
    }
}

impl RdpConfig {
    /// A new RDP connection on the protocol's default port.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError` when the name or address is empty, the
    /// address contains whitespace or the optional user is unusable.
    pub fn new(
        name: impl Into<String>,
        address: impl Into<String>,
        user: Option<impl Into<String>>,
    ) -> Result<Self, ConnectionValidationError> {
        Ok(Self {
            id: HostId::new(),
            name: connection_name(name.into())?,
            group: None,
            group_path: None,
            address: connection_address(address.into())?,
            port: Port(DEFAULT_RDP_PORT),
            user: connection_user(user.map(Into::into))?,
            domain: None,
            certificate: RdpCertificatePolicy::default(),
            remember_secret: false,
            clipboard: true,
            drive_folder: None,
        })
    }

    /// Stable identifier, shared with the secret store key and the workspace.
    #[must_use]
    pub fn id(&self) -> HostId {
        self.id
    }

    /// User-visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Host name or address.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// TCP port.
    #[must_use]
    pub fn port(&self) -> Port {
        self.port
    }

    /// The user the next connection signs in as, when one was chosen.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// The domain the user belongs to, when one was chosen.
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// The single grouping label an earlier build would have written.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The hierarchy path of the group this connection is in.
    #[must_use]
    pub fn group_path(&self) -> Option<&GroupPath> {
        self.group_path.as_ref()
    }

    /// What the connection does with a certificate it does not already trust.
    #[must_use]
    pub fn certificate(&self) -> RdpCertificatePolicy {
        self.certificate
    }

    /// Whether the clipboard is shared with the remote desktop.
    #[must_use]
    pub fn clipboard(&self) -> bool {
        self.clipboard
    }

    /// The folder on this machine the remote desktop sees as a drive, if any.
    #[must_use]
    pub fn drive_folder(&self) -> Option<&str> {
        self.drive_folder.as_deref()
    }

    /// Whether a credential for this identifier may be in the system store.
    #[must_use]
    pub fn remembers_secret(&self) -> bool {
        self.remember_secret
    }

    /// What the connection would dial, as one line for the list.
    #[must_use]
    pub fn target(&self) -> String {
        match self.user() {
            Some(user) => format!("{user}@{}:{}", self.address, self.port),
            None => format!("{}:{}", self.address, self.port),
        }
    }

    /// Renames the connection.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::EmptyName` for a blank name.
    pub fn set_name(&mut self, name: impl Into<String>) -> Result<(), ConnectionValidationError> {
        self.name = connection_name(name.into())?;
        Ok(())
    }

    /// Changes the address.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::EmptyAddress` or
    /// `ConnectionValidationError::AddressContainsWhitespace`.
    pub fn set_address(
        &mut self,
        address: impl Into<String>,
    ) -> Result<(), ConnectionValidationError> {
        self.address = connection_address(address.into())?;
        Ok(())
    }

    /// Changes the port.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::PortOutOfRange` outside `1..=65535`.
    pub fn set_port(&mut self, port: u32) -> Result<(), ConnectionValidationError> {
        self.port = connection_port(port)?;
        Ok(())
    }

    /// Sets or clears the user.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::UserContainsWhitespace` for a user with
    /// whitespace in it.
    pub fn set_user(
        &mut self,
        user: Option<impl Into<String>>,
    ) -> Result<(), ConnectionValidationError> {
        self.user = connection_user(user.map(Into::into))?;
        Ok(())
    }

    /// Sets or clears the domain.
    pub fn set_domain(&mut self, domain: Option<impl Into<String>>) {
        self.domain = normalize_optional_text(domain.map(Into::into));
    }

    /// Chooses what an untrusted certificate means for this connection.
    pub fn set_certificate(&mut self, policy: RdpCertificatePolicy) {
        self.certificate = policy;
    }

    /// Turns clipboard sharing with the remote desktop on or off.
    pub fn set_clipboard(&mut self, clipboard: bool) {
        self.clipboard = clipboard;
    }

    /// Chooses the folder the remote desktop sees as a drive; a blank path turns
    /// the mapping off.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::DriveFolderContainsControlCharacter`
    /// for a path with a control character, leaving the previous folder in place.
    pub fn set_drive_folder(
        &mut self,
        folder: Option<impl Into<String>>,
    ) -> Result<(), ConnectionValidationError> {
        self.drive_folder = connection_drive_folder(folder.map(Into::into))?;
        Ok(())
    }

    /// Sets or clears the grouping label.
    pub fn set_group(&mut self, group: Option<impl Into<String>>) {
        self.group = normalize_optional_text(group.map(Into::into));
        if self.group.is_some() {
            self.group_path = None;
        }
    }

    /// Places the connection in a group hierarchy.
    pub fn set_group_path(&mut self, path: Option<GroupPath>) {
        if path.is_some() {
            self.group = None;
        }
        self.group_path = path;
    }

    /// Records or clears whether the system store may hold a credential for this id.
    pub fn set_remember_secret(&mut self, remember: bool) {
        self.remember_secret = remember;
    }
}

/// A saved VNC connection (R18).
///
/// VNC authentication is a password and nothing else, so this record has no user
/// and no domain field rather than two fields that would always be empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "VncConfigWire")]
pub struct VncConfig {
    id: HostId,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_path: Option<GroupPath>,
    address: String,
    port: Port,
    /// How much this connection insists on encryption.
    #[serde(default, skip_serializing_if = "security_is_default")]
    security: VncSecurity,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    remember_secret: bool,
    /// Whether text copied on either side can be pasted on the other.
    #[serde(
        default = "clipboard_default",
        skip_serializing_if = "clipboard_is_default"
    )]
    clipboard: bool,
}

/// The stored shape of a VNC connection.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VncConfigWire {
    id: HostId,
    name: String,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    group_path: Option<GroupPath>,
    address: String,
    port: Port,
    #[serde(default)]
    security: VncSecurity,
    #[serde(default)]
    remember_secret: bool,
    #[serde(default = "clipboard_default")]
    clipboard: bool,
}

impl TryFrom<VncConfigWire> for VncConfig {
    type Error = ConnectionValidationError;

    fn try_from(wire: VncConfigWire) -> Result<Self, Self::Error> {
        let (group, group_path) = split_group(wire.group, wire.group_path);
        Ok(Self {
            id: wire.id,
            name: connection_name(wire.name)?,
            group,
            group_path,
            address: connection_address(wire.address)?,
            port: connection_port(u32::from(wire.port.get()))?,
            security: wire.security,
            remember_secret: wire.remember_secret,
            clipboard: wire.clipboard,
        })
    }
}

impl VncConfig {
    /// A new VNC connection on the protocol's default port.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError` when the name or address is empty or the
    /// address contains whitespace.
    pub fn new(
        name: impl Into<String>,
        address: impl Into<String>,
    ) -> Result<Self, ConnectionValidationError> {
        Ok(Self {
            id: HostId::new(),
            name: connection_name(name.into())?,
            group: None,
            group_path: None,
            address: connection_address(address.into())?,
            port: Port(DEFAULT_VNC_PORT),
            security: VncSecurity::default(),
            remember_secret: false,
            clipboard: true,
        })
    }

    /// Stable identifier, shared with the secret store key and the workspace.
    #[must_use]
    pub fn id(&self) -> HostId {
        self.id
    }

    /// User-visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Host name or address.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// TCP port.
    #[must_use]
    pub fn port(&self) -> Port {
        self.port
    }

    /// The single grouping label an earlier build would have written.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The hierarchy path of the group this connection is in.
    #[must_use]
    pub fn group_path(&self) -> Option<&GroupPath> {
        self.group_path.as_ref()
    }

    /// How much this connection insists on encryption.
    #[must_use]
    pub fn security(&self) -> VncSecurity {
        self.security
    }

    /// Whether the clipboard is shared with the remote desktop.
    #[must_use]
    pub fn clipboard(&self) -> bool {
        self.clipboard
    }

    /// Turns clipboard sharing with the remote desktop on or off.
    pub fn set_clipboard(&mut self, clipboard: bool) {
        self.clipboard = clipboard;
    }

    /// Whether a credential for this identifier may be in the system store.
    #[must_use]
    pub fn remembers_secret(&self) -> bool {
        self.remember_secret
    }

    /// What the connection would dial, as one line for the list.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    /// Renames the connection.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::EmptyName` for a blank name.
    pub fn set_name(&mut self, name: impl Into<String>) -> Result<(), ConnectionValidationError> {
        self.name = connection_name(name.into())?;
        Ok(())
    }

    /// Changes the address.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::EmptyAddress` or
    /// `ConnectionValidationError::AddressContainsWhitespace`.
    pub fn set_address(
        &mut self,
        address: impl Into<String>,
    ) -> Result<(), ConnectionValidationError> {
        self.address = connection_address(address.into())?;
        Ok(())
    }

    /// Changes the port.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::PortOutOfRange` outside `1..=65535`.
    pub fn set_port(&mut self, port: u32) -> Result<(), ConnectionValidationError> {
        self.port = connection_port(port)?;
        Ok(())
    }

    /// Sets or clears the grouping label.
    pub fn set_group(&mut self, group: Option<impl Into<String>>) {
        self.group = normalize_optional_text(group.map(Into::into));
        if self.group.is_some() {
            self.group_path = None;
        }
    }

    /// Places the connection in a group hierarchy.
    pub fn set_group_path(&mut self, path: Option<GroupPath>) {
        if path.is_some() {
            self.group = None;
        }
        self.group_path = path;
    }

    /// Chooses how much this connection insists on encryption.
    pub fn set_security(&mut self, security: VncSecurity) {
        self.security = security;
    }

    /// Records or clears whether the system store may hold a credential for this id.
    pub fn set_remember_secret(&mut self, remember: bool) {
        self.remember_secret = remember;
    }
}

/// A saved serial connection (R18, R21).
///
/// The record names the device and the frame it speaks. It holds no credential:
/// a serial port authenticates nothing, so there is no field here that could carry
/// a secret and no `remember_secret` flag to mislead a reader.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SerialConfigWire")]
pub struct SerialConfig {
    id: HostId,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_path: Option<GroupPath>,
    /// The device path or port name, exactly as the platform spells it.
    device: String,
    baud: u32,
    data_bits: u8,
    parity: SerialParity,
    stop_bits: SerialStopBits,
    flow: SerialFlow,
    /// Whether what the user types is echoed locally instead of by the device.
    ///
    /// A terminal device that does not echo would otherwise show nothing as it is
    /// typed; a device that does would show every character twice, so this is a
    /// per-connection decision and not a global one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    local_echo: bool,
}

/// The stored shape of a serial connection.
///
/// The frame parameters default to the values a new form starts with, so a record
/// that names only a device still means something specific instead of failing to
/// load.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SerialConfigWire {
    id: HostId,
    name: String,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    group_path: Option<GroupPath>,
    device: String,
    #[serde(default = "default_baud")]
    baud: u32,
    #[serde(default = "default_data_bits")]
    data_bits: u8,
    #[serde(default)]
    parity: SerialParity,
    #[serde(default)]
    stop_bits: SerialStopBits,
    #[serde(default)]
    flow: SerialFlow,
    #[serde(default)]
    local_echo: bool,
}

fn default_baud() -> u32 {
    DEFAULT_SERIAL_BAUD
}

fn default_data_bits() -> u8 {
    MAX_SERIAL_DATA_BITS
}

impl TryFrom<SerialConfigWire> for SerialConfig {
    type Error = ConnectionValidationError;

    fn try_from(wire: SerialConfigWire) -> Result<Self, Self::Error> {
        let (group, group_path) = split_group(wire.group, wire.group_path);
        Ok(Self {
            id: wire.id,
            name: connection_name(wire.name)?,
            group,
            group_path,
            device: connection_device(wire.device)?,
            baud: connection_baud(wire.baud)?,
            data_bits: connection_data_bits(wire.data_bits)?,
            parity: wire.parity,
            stop_bits: wire.stop_bits,
            flow: wire.flow,
            local_echo: wire.local_echo,
        })
    }
}

impl SerialConfig {
    /// A new serial connection on the default line speed and frame.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError` when the name is empty or the device is
    /// empty or carries control characters.
    pub fn new(
        name: impl Into<String>,
        device: impl Into<String>,
    ) -> Result<Self, ConnectionValidationError> {
        Ok(Self {
            id: HostId::new(),
            name: connection_name(name.into())?,
            group: None,
            group_path: None,
            device: connection_device(device.into())?,
            baud: DEFAULT_SERIAL_BAUD,
            data_bits: MAX_SERIAL_DATA_BITS,
            parity: SerialParity::None,
            stop_bits: SerialStopBits::One,
            flow: SerialFlow::None,
            local_echo: false,
        })
    }

    /// Stable identifier, shared with the workspace.
    #[must_use]
    pub fn id(&self) -> HostId {
        self.id
    }

    /// User-visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The device this connection opens, exactly as the platform spells it.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Line speed.
    #[must_use]
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// Bits per frame.
    #[must_use]
    pub fn data_bits(&self) -> u8 {
        self.data_bits
    }

    /// Parity.
    #[must_use]
    pub fn parity(&self) -> SerialParity {
        self.parity
    }

    /// Stop bits.
    #[must_use]
    pub fn stop_bits(&self) -> SerialStopBits {
        self.stop_bits
    }

    /// Flow control.
    #[must_use]
    pub fn flow(&self) -> SerialFlow {
        self.flow
    }

    /// Whether typed characters are echoed locally rather than by the device.
    #[must_use]
    pub fn local_echo(&self) -> bool {
        self.local_echo
    }

    /// The single grouping label an earlier build would have written.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The hierarchy path of the group this connection is in.
    #[must_use]
    pub fn group_path(&self) -> Option<&GroupPath> {
        self.group_path.as_ref()
    }

    /// What the connection would open, as one line for the list.
    ///
    /// The device and its line speed are what a person recognises a port by; the frame
    /// follows it so the row still says how the bytes are shaped.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}@{} {}", self.device, self.baud, self.frame())
    }

    /// The frame parameters as the short form a serial tool prints.
    #[must_use]
    pub fn frame(&self) -> String {
        format!(
            "{}{}{}",
            self.data_bits,
            match self.parity {
                SerialParity::None => "N",
                SerialParity::Even => "E",
                SerialParity::Odd => "O",
                SerialParity::Mark => "M",
                SerialParity::Space => "S",
            },
            self.stop_bits.label()
        )
    }

    /// Renames the connection.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::EmptyName` for a blank name.
    pub fn set_name(&mut self, name: impl Into<String>) -> Result<(), ConnectionValidationError> {
        self.name = connection_name(name.into())?;
        Ok(())
    }

    /// Changes the device.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::EmptyDevice` or
    /// `ConnectionValidationError::DeviceContainsControlCharacter`.
    pub fn set_device(
        &mut self,
        device: impl Into<String>,
    ) -> Result<(), ConnectionValidationError> {
        self.device = connection_device(device.into())?;
        Ok(())
    }

    /// Changes the line speed.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::BaudOutOfRange` outside the supported range.
    pub fn set_baud(&mut self, baud: u32) -> Result<(), ConnectionValidationError> {
        self.baud = connection_baud(baud)?;
        Ok(())
    }

    /// Changes the frame width.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionValidationError::DataBitsOutOfRange` outside `5..=8`.
    pub fn set_data_bits(&mut self, data_bits: u8) -> Result<(), ConnectionValidationError> {
        self.data_bits = connection_data_bits(data_bits)?;
        Ok(())
    }

    /// Sets the parity.
    pub fn set_parity(&mut self, parity: SerialParity) {
        self.parity = parity;
    }

    /// Sets the number of stop bits.
    pub fn set_stop_bits(&mut self, stop_bits: SerialStopBits) {
        self.stop_bits = stop_bits;
    }

    /// Sets the flow control.
    pub fn set_flow(&mut self, flow: SerialFlow) {
        self.flow = flow;
    }

    /// Chooses whether typed characters are echoed locally.
    pub fn set_local_echo(&mut self, echo: bool) {
        self.local_echo = echo;
    }

    /// Sets or clears the grouping label.
    pub fn set_group(&mut self, group: Option<impl Into<String>>) {
        self.group = normalize_optional_text(group.map(Into::into));
        if self.group.is_some() {
            self.group_path = None;
        }
    }

    /// Places the connection in a group hierarchy.
    pub fn set_group_path(&mut self, path: Option<GroupPath>) {
        if path.is_some() {
            self.group = None;
        }
        self.group_path = path;
    }
}

/// One saved connection of any protocol (R2, R18).
///
/// The SSH variant holds the record this build already wrote, so an SSH connection
/// keeps every field it had; the other variants hold only their own protocol's
/// configuration. Nothing here is a live session, and nothing here dials, opens a
/// device or runs a command.
///
/// The stored form carries the protocol as a tag, except for SSH, which is written
/// exactly as before so an older file stays byte-identical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ConnectionRecord {
    /// A saved SSH host, with the fields this build already had.
    Ssh(HostConfig),
    /// A saved RDP endpoint.
    Rdp(RdpConfig),
    /// A saved VNC endpoint.
    Vnc(VncConfig),
    /// A saved serial device.
    Serial(SerialConfig),
}

impl ConnectionRecord {
    /// Stable identifier, shared by the list, the secret store and the workspace.
    #[must_use]
    pub fn id(&self) -> HostId {
        match self {
            Self::Ssh(host) => host.id(),
            Self::Rdp(config) => config.id(),
            Self::Vnc(config) => config.id(),
            Self::Serial(config) => config.id(),
        }
    }

    /// User-visible name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Ssh(host) => host.name(),
            Self::Rdp(config) => config.name(),
            Self::Vnc(config) => config.name(),
            Self::Serial(config) => config.name(),
        }
    }

    /// Which protocol this connection speaks.
    #[must_use]
    pub fn protocol(&self) -> ProtocolKind {
        match self {
            Self::Ssh(_) => ProtocolKind::Ssh,
            Self::Rdp(_) => ProtocolKind::Rdp,
            Self::Vnc(_) => ProtocolKind::Vnc,
            Self::Serial(_) => ProtocolKind::Serial,
        }
    }

    /// The single grouping label an earlier build would have written.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        match self {
            Self::Ssh(host) => host.group(),
            Self::Rdp(config) => config.group(),
            Self::Vnc(config) => config.group(),
            Self::Serial(config) => config.group(),
        }
    }

    /// The hierarchy path of the group this connection is in.
    ///
    /// Returned by value because the SSH record already answers with its own copy;
    /// a caller that needs one string should ask [`Self::group_text`].
    #[must_use]
    pub fn group_path(&self) -> Option<GroupPath> {
        match self {
            Self::Ssh(host) => host.group_path(),
            Self::Rdp(config) => config.group_path().cloned(),
            Self::Vnc(config) => config.group_path().cloned(),
            Self::Serial(config) => config.group_path().cloned(),
        }
    }

    /// The grouping as one string, whichever of the two spellings the record has.
    #[must_use]
    pub fn group_text(&self) -> Option<String> {
        self.group_path()
            .map(|path| path.to_string())
            .or_else(|| self.group().map(str::to_owned))
    }

    /// What the connection would reach, as one line for the list.
    #[must_use]
    pub fn target(&self) -> String {
        match self {
            Self::Ssh(host) => host.target(),
            Self::Rdp(config) => config.target(),
            Self::Vnc(config) => config.target(),
            Self::Serial(config) => config.target(),
        }
    }

    /// Whether a credential for this identifier may be in the system store.
    ///
    /// A serial port never has one. Desktop and SSH connections record only the
    /// fact that the store may hold a secret; the secret itself stays there.
    #[must_use]
    pub fn remembers_secret(&self) -> bool {
        match self {
            Self::Ssh(host) => host.remembers_secret(),
            Self::Rdp(config) => config.remembers_secret(),
            Self::Vnc(config) => config.remembers_secret(),
            Self::Serial(_) => false,
        }
    }

    /// The SSH record, when this connection is one.
    ///
    /// Batch targets, SSH imports, forwarding rules and X11 sessions ask for this
    /// and get `None` for every other protocol, which is what keeps those features
    /// off a connection they cannot use.
    #[must_use]
    pub fn as_ssh(&self) -> Option<&HostConfig> {
        match self {
            Self::Ssh(host) => Some(host),
            Self::Rdp(_) | Self::Vnc(_) | Self::Serial(_) => None,
        }
    }

    /// Whether this connection is an SSH one.
    #[must_use]
    pub fn is_ssh(&self) -> bool {
        self.protocol().is_ssh()
    }

    /// What a session of this protocol can do (R2, R18, R23).
    #[must_use]
    pub fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities::of_protocol(self.protocol())
    }

    /// A copy with a fresh identifier, no saved credential and no history (R9).
    ///
    /// The copy is a new record: the recency log is keyed by identifier, so the
    /// original's history does not follow it, and no credential reference is
    /// carried because the copy has an identifier the store has never seen.
    /// An SSH copy also drops any jump that named the copy itself, so a duplicate
    /// can never be built as a cycle. Everything else is kept, including a desktop
    /// connection's clipboard choice and mapped folder: the copy is made on this
    /// machine, where that folder means the same thing.
    #[must_use]
    pub fn duplicated(&self) -> Self {
        match self {
            Self::Ssh(host) => {
                let mut copy = host.clone();
                copy.id = HostId::new();
                copy.remember_secret = false;
                let id = copy.id;
                copy.jumps.retain(|jump| *jump != id);
                Self::Ssh(copy)
            }
            Self::Rdp(config) => {
                let mut copy = config.clone();
                copy.id = HostId::new();
                copy.remember_secret = false;
                Self::Rdp(copy)
            }
            Self::Vnc(config) => {
                let mut copy = config.clone();
                copy.id = HostId::new();
                copy.remember_secret = false;
                Self::Vnc(copy)
            }
            Self::Serial(config) => {
                let mut copy = config.clone();
                copy.id = HostId::new();
                Self::Serial(copy)
            }
        }
    }

    /// The same connection under another identifier.
    ///
    /// A record keeps the identifier the credential store and the workspace know.
    /// Changing protocol therefore means building the new record and giving it the
    /// identifier the old one had, which is what this does; a fresh identifier would
    /// look like a different connection to both.
    #[must_use]
    pub fn reidentified(self, id: HostId) -> Self {
        match self {
            Self::Ssh(mut host) => {
                host.id = id;
                Self::Ssh(host)
            }
            Self::Rdp(mut config) => {
                config.id = id;
                Self::Rdp(config)
            }
            Self::Vnc(mut config) => {
                config.id = id;
                Self::Vnc(config)
            }
            Self::Serial(mut config) => {
                config.id = id;
                Self::Serial(config)
            }
        }
    }

    /// The same connection in another group, or in none.
    ///
    /// Only the grouping changes: the identifier, the endpoint and any credential
    /// reference stay, which is what lets a group rename or a group deletion move a
    /// desktop or serial connection exactly the way it moves a host.
    #[must_use]
    pub fn with_group_path(self, path: Option<GroupPath>) -> Self {
        match self {
            Self::Ssh(mut host) => {
                host.set_group_path(path);
                Self::Ssh(host)
            }
            Self::Rdp(mut config) => {
                config.set_group_path(path);
                Self::Rdp(config)
            }
            Self::Vnc(mut config) => {
                config.set_group_path(path);
                Self::Vnc(config)
            }
            Self::Serial(mut config) => {
                config.set_group_path(path);
                Self::Serial(config)
            }
        }
    }

    /// Renames the connection.
    ///
    /// # Errors
    ///
    /// Returns the protocol's own refusal for a blank name or an unusable field.
    pub fn set_name(&mut self, name: impl Into<String>) -> Result<(), ConnectionValidationError> {
        let name = name.into();
        match self {
            Self::Ssh(host) => host.set_name(name).map_err(host_error),
            Self::Rdp(config) => config.set_name(name),
            Self::Vnc(config) => config.set_name(name),
            Self::Serial(config) => config.set_name(name),
        }
    }

    /// Whether this connection answers a search phrase (R2).
    ///
    /// The phrase is matched case-insensitively against the name, the protocol name
    /// and what the connection would reach, so "rdp", "office" and "10.0.0.5:3389"
    /// all find the same record.
    #[must_use]
    pub fn matches(&self, phrase: &str) -> bool {
        let phrase = phrase.trim().to_lowercase();
        if phrase.is_empty() {
            return true;
        }
        let protocol = self.protocol();
        self.name().to_lowercase().contains(&phrase)
            || protocol.label().to_lowercase().contains(&phrase)
            || protocol.wire_name().contains(&phrase)
            || self.target().to_lowercase().contains(&phrase)
    }
}

impl From<HostConfig> for ConnectionRecord {
    fn from(host: HostConfig) -> Self {
        Self::Ssh(host)
    }
}

/// Names the shared refusal for a name the SSH record rejected.
///
/// The SSH record has its own error type because it has more required fields; a
/// caller that switches on protocol should still see one error shape.
fn host_error(error: HostValidationError) -> ConnectionValidationError {
    match error {
        HostValidationError::EmptyName => ConnectionValidationError::EmptyName,
        HostValidationError::EmptyAddress => ConnectionValidationError::EmptyAddress,
        HostValidationError::AddressContainsWhitespace => {
            ConnectionValidationError::AddressContainsWhitespace
        }
        HostValidationError::EmptyUser => ConnectionValidationError::EmptyName,
        HostValidationError::PortOutOfRange { value } => {
            ConnectionValidationError::PortOutOfRange { value }
        }
    }
}

/// Splits the two group spellings the way the SSH record does: a stored path wins.
fn split_group(
    group: Option<String>,
    group_path: Option<GroupPath>,
) -> (Option<String>, Option<GroupPath>) {
    match group_path {
        Some(path) => (None, Some(path)),
        None => (normalize_optional_text(group), None),
    }
}

/// Trims an optional piece of text and turns a blank one into `None`.
fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn connection_name(name: String) -> Result<String, ConnectionValidationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ConnectionValidationError::EmptyName);
    }
    Ok(trimmed.to_owned())
}

fn connection_address(address: String) -> Result<String, ConnectionValidationError> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err(ConnectionValidationError::EmptyAddress);
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(ConnectionValidationError::AddressContainsWhitespace);
    }
    Ok(trimmed.to_owned())
}

fn connection_port(port: u32) -> Result<Port, ConnectionValidationError> {
    Port::new(port).map_err(|_| ConnectionValidationError::PortOutOfRange { value: port })
}

fn connection_user(user: Option<String>) -> Result<Option<String>, ConnectionValidationError> {
    let user = normalize_optional_text(user);
    let unusable = user
        .as_deref()
        .is_some_and(|user| user.chars().any(char::is_whitespace));
    if unusable {
        return Err(ConnectionValidationError::UserContainsWhitespace);
    }
    Ok(user)
}

fn connection_device(device: String) -> Result<String, ConnectionValidationError> {
    let trimmed = device.trim();
    if trimmed.is_empty() {
        return Err(ConnectionValidationError::EmptyDevice);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ConnectionValidationError::DeviceContainsControlCharacter);
    }
    Ok(trimmed.to_owned())
}

fn connection_baud(baud: u32) -> Result<u32, ConnectionValidationError> {
    if !(MIN_SERIAL_BAUD..=MAX_SERIAL_BAUD).contains(&baud) {
        return Err(ConnectionValidationError::BaudOutOfRange { value: baud });
    }
    Ok(baud)
}

fn connection_data_bits(data_bits: u8) -> Result<u8, ConnectionValidationError> {
    if !(MIN_SERIAL_DATA_BITS..=MAX_SERIAL_DATA_BITS).contains(&data_bits) {
        return Err(ConnectionValidationError::DataBitsOutOfRange { value: data_bits });
    }
    Ok(data_bits)
}

/// Why a jump chain cannot be used, limited to the hop that is wrong.
///
/// The host records themselves stay loaded. A fault names the host a person can
/// edit and the change that makes the chain usable; it does not mean a connection
/// was attempted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JumpFault {
    /// The host being connected is not in the saved set.
    #[error("{message}")]
    UnknownHost {
        /// The id that was asked for.
        host: HostId,
        /// What to do next. Never includes a secret.
        message: String,
    },
    /// One hop points at a host that is not saved.
    #[error("{message}")]
    MissingHop {
        /// The host whose jump list contains the bad reference.
        host: HostId,
        /// Zero-based position of the bad hop in that host's list.
        index: usize,
        /// The id that is not saved.
        missing: HostId,
        /// What to do next. Never includes a secret.
        message: String,
    },
    /// Following the chain returns to a host already on the path.
    #[error("{message}")]
    Cycle {
        /// The loop, with the first host repeated at the end.
        path: Vec<HostId>,
        /// What to do next. Never includes a secret.
        message: String,
    },
}

impl JumpFault {
    /// The explanation a person can act on.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::UnknownHost { message, .. }
            | Self::MissingHop { message, .. }
            | Self::Cycle { message, .. } => message,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    White,
    Gray,
    Black,
}

fn index_hosts(hosts: &[HostConfig]) -> HashMap<HostId, &HostConfig> {
    hosts.iter().map(|host| (host.id(), host)).collect()
}

fn host_label(id: HostId, hosts: &HashMap<HostId, &HostConfig>) -> String {
    match hosts.get(&id) {
        Some(host) => format!("\"{}\" ({id})", host.name()),
        None => id.to_string(),
    }
}

/// Every direct jump problem in `hosts`.
///
/// A missing hop is reported on the host that names it. A cycle is reported
/// once, as the loop itself. Hosts that are not part of either stay out of the
/// list, and the records are not modified.
#[must_use]
pub fn jump_faults(hosts: &[HostConfig]) -> Vec<JumpFault> {
    let by_id = index_hosts(hosts);
    let mut faults = Vec::new();
    let mut color: HashMap<HostId, Visit> = HashMap::new();
    let mut stack = Vec::new();
    for host in hosts {
        if color.get(&host.id()).copied().unwrap_or(Visit::White) == Visit::White {
            trace_jumps(host.id(), &by_id, &mut color, &mut stack, &mut faults);
        }
    }
    faults
}

/// The first jump problem on the way to `target`, if connecting it would be invalid.
///
/// `None` means the chain resolves. This does not open a socket or a process.
#[must_use]
pub fn connection_jump_fault(hosts: &[HostConfig], target: HostId) -> Option<JumpFault> {
    let by_id = index_hosts(hosts);
    let mut stack = Vec::new();
    follow(target, target, &by_id, &mut stack).err()
}

fn trace_jumps(
    id: HostId,
    by_id: &HashMap<HostId, &HostConfig>,
    color: &mut HashMap<HostId, Visit>,
    stack: &mut Vec<HostId>,
    faults: &mut Vec<JumpFault>,
) {
    color.insert(id, Visit::Gray);
    stack.push(id);
    if let Some(host) = by_id.get(&id) {
        for (index, hop) in host.jumps().iter().enumerate() {
            if !by_id.contains_key(hop) {
                faults.push(missing_hop(id, id, index, *hop, by_id));
                continue;
            }
            match color.get(hop).copied().unwrap_or(Visit::White) {
                Visit::Gray => {
                    let start = stack.iter().position(|item| item == hop).unwrap_or(0);
                    let mut path = stack[start..].to_vec();
                    path.push(*hop);
                    faults.push(cycle_fault(id, &path, by_id));
                }
                Visit::White => trace_jumps(*hop, by_id, color, stack, faults),
                Visit::Black => {}
            }
        }
    }
    stack.pop();
    color.insert(id, Visit::Black);
}

fn follow(
    origin: HostId,
    current: HostId,
    by_id: &HashMap<HostId, &HostConfig>,
    stack: &mut Vec<HostId>,
) -> Result<(), JumpFault> {
    if let Some(start) = stack.iter().position(|id| *id == current) {
        let mut path = stack[start..].to_vec();
        path.push(current);
        return Err(cycle_fault(origin, &path, by_id));
    }
    let Some(host) = by_id.get(&current) else {
        return Err(if stack.is_empty() {
            JumpFault::UnknownHost {
                host: current,
                message: format!(
                    "Host {current} is not saved, so it cannot be connected. Choose a saved host. Nothing was connected."
                ),
            }
        } else {
            missing_hop(origin, *stack.last().unwrap_or(&current), 0, current, by_id)
        });
    };
    stack.push(current);
    for (index, hop) in host.jumps().iter().enumerate() {
        if !by_id.contains_key(hop) {
            return Err(missing_hop(origin, current, index, *hop, by_id));
        }
        follow(origin, *hop, by_id, stack)?;
    }
    stack.pop();
    Ok(())
}

fn missing_hop(
    origin: HostId,
    owner: HostId,
    index: usize,
    missing: HostId,
    by_id: &HashMap<HostId, &HostConfig>,
) -> JumpFault {
    let hop_number = index + 1;
    let owner_label = host_label(owner, by_id);
    let message = if origin == owner {
        format!(
            "{owner_label} jump {hop_number} references host {missing}, which is not saved. Remove that hop or restore the host before connecting. Nothing was connected."
        )
    } else {
        format!(
            "{} cannot be connected because {owner_label} jump {hop_number} references host {missing}, which is not saved. Remove that hop or restore the host. Nothing was connected.",
            host_label(origin, by_id)
        )
    };
    JumpFault::MissingHop {
        host: owner,
        index,
        missing,
        message,
    }
}

fn cycle_fault(origin: HostId, path: &[HostId], by_id: &HashMap<HostId, &HostConfig>) -> JumpFault {
    let rendered = path
        .iter()
        .map(|id| host_label(*id, by_id))
        .collect::<Vec<_>>()
        .join(" -> ");
    let origin_label = host_label(origin, by_id);
    let message = if path.contains(&origin) {
        format!(
            "Jump chain loops: {rendered}. Remove the hop that returns to an earlier host before connecting. Nothing was connected."
        )
    } else {
        format!(
            "{origin_label} cannot be connected because the jump chain loops: {rendered}. Remove the hop that returns to an earlier host. Nothing was connected."
        )
    };
    JumpFault::Cycle {
        path: path.to_vec(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hosts_keep_the_values_they_were_given() {
        let host = HostConfig::new("  Build server  ", " 10.0.0.7 ", 2222, " deploy ")
            .expect("valid host");

        assert_eq!(host.name(), "Build server");
        assert_eq!(host.address(), "10.0.0.7");
        assert_eq!(host.port().get(), 2222);
        assert_eq!(host.user(), "deploy");
        assert_eq!(host.target(), "deploy@10.0.0.7:2222");
        assert!(host.group().is_none());
    }

    #[test]
    fn blank_fields_are_rejected() {
        assert_eq!(
            HostConfig::new("   ", "example.com", 22, "root"),
            Err(HostValidationError::EmptyName)
        );
        assert_eq!(
            HostConfig::new("web", "  ", 22, "root"),
            Err(HostValidationError::EmptyAddress)
        );
        assert_eq!(
            HostConfig::new("web", "example.com", 22, ""),
            Err(HostValidationError::EmptyUser)
        );
        assert_eq!(
            HostConfig::new("web", "two words", 22, "root"),
            Err(HostValidationError::AddressContainsWhitespace)
        );
    }

    #[test]
    fn ports_outside_the_tcp_range_are_rejected_with_the_offending_value() {
        assert_eq!(
            HostConfig::new("web", "example.com", 0, "root"),
            Err(HostValidationError::PortOutOfRange { value: 0 })
        );
        assert_eq!(
            Port::new(65_536),
            Err(HostValidationError::PortOutOfRange { value: 65_536 })
        );
        assert_eq!(Port::new(65_535).expect("65535 is valid").get(), 65_535);
        assert_eq!(Port::default(), Port::DEFAULT);
    }

    #[test]
    fn edits_are_validated_before_they_replace_the_current_value() {
        let mut host = HostConfig::new("web", "example.com", 22, "root").expect("valid host");

        assert_eq!(
            host.set_port(0),
            Err(HostValidationError::PortOutOfRange { value: 0 })
        );
        assert_eq!(host.port().get(), 22, "a rejected edit keeps the old port");

        host.set_address("example.org").expect("valid address");
        host.set_user("ops").expect("valid user");
        host.set_name("prod web").expect("valid name");
        assert_eq!(host.target(), "ops@example.org:22");
    }

    #[test]
    fn deserialization_runs_the_same_validation_as_construction() {
        let host = HostConfig::new("web", "example.com", 22, "root").expect("valid host");
        let encoded = serde_json::to_string(&host).expect("host serializes");
        let decoded: HostConfig = serde_json::from_str(&encoded).expect("host round trips");
        assert_eq!(decoded, host);

        let invalid = format!(
            r#"{{"id":"{}","name":"web","address":"example.com","port":0,"user":"root","group":null}}"#,
            host.id()
        );
        assert!(serde_json::from_str::<HostConfig>(&invalid).is_err());
    }
    #[test]
    fn deserialization_refuses_fields_this_build_does_not_write() {
        // A stored host that carries a password is not a record this build wrote.
        // Reading it would accept a credential into a model that has no field for
        // one, so the whole record is rejected instead.
        let host = HostConfig::new("web", "example.com", 22, "root").expect("valid host");
        let with_password = format!(
            r#"{{"id":"{}","name":"web","address":"example.com","port":22,"user":"root","group":null,"password":"hunter2"}}"#,
            host.id()
        );
        let error = serde_json::from_str::<HostConfig>(&with_password)
            .expect_err("a host record with a password must not load");
        assert!(
            error.to_string().contains("unknown field"),
            "the reason must name the problem: {error}"
        );
    }

    #[test]
    fn a_stored_host_cannot_carry_a_secret_in_its_own_text() {
        // The guarantee is structural rather than a scrubber: the serialized form
        // has exactly the six fields above, so there is nowhere for a password or a
        // key passphrase to be written.
        let host = HostConfig::new("web", "example.com", 22, "root").expect("valid host");
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&host).expect("host serializes"))
                .expect("valid json");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("a host is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["address", "group", "id", "name", "port", "user"],
            "a saved host may not grow a field that could hold a secret"
        );
    }

    #[test]
    fn an_imported_key_path_is_a_path_and_nothing_else() {
        let mut host = HostConfig::new("web", "example.com", 22, "root").expect("valid host");
        host.set_identity_file(Some("  /home/deploy/.ssh/id_ed25519  "));
        assert_eq!(host.identity_file(), Some("/home/deploy/.ssh/id_ed25519"));

        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&host).expect("host serializes"))
                .expect("valid json");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("a host is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "address",
                "group",
                "id",
                "identity_file",
                "name",
                "port",
                "user"
            ],
            "a key path adds exactly one field, and it holds a path"
        );
        assert_eq!(
            value["identity_file"],
            serde_json::json!("/home/deploy/.ssh/id_ed25519"),
            "the stored value is the path the file named, not key material"
        );

        // A blank path is no path: an editor that submits an empty field clears it
        // instead of recording something that looks like a key.
        host.set_identity_file(Some("   "));
        assert_eq!(host.identity_file(), None);
        host.set_identity_file(Some("C:/keys/work key.pem"));
        assert_eq!(
            host.identity_file(),
            Some("C:/keys/work key.pem"),
            "a path with a space stays one path"
        );
    }

    #[test]
    fn a_host_file_written_before_key_paths_existed_still_loads() {
        let host = HostConfig::new("web", "example.com", 22, "root").expect("valid host");
        let without_field = format!(
            r#"{{"id":"{}","name":"web","address":"example.com","port":22,"user":"root","group":null}}"#,
            host.id()
        );
        let decoded: HostConfig =
            serde_json::from_str(&without_field).expect("an older record still loads");
        assert_eq!(decoded, host);
        assert_eq!(decoded.identity_file(), None);
    }
}

#[cfg(test)]
mod sign_in_tests {
    use super::*;

    fn host() -> HostConfig {
        HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("valid")
    }

    #[test]
    fn a_host_without_the_new_fields_reads_and_writes_as_before() {
        let host = host();
        let json = serde_json::to_string(&host).expect("serializes");
        for field in [
            "auth",
            "remember_secret",
            "startup_command",
            "jumps",
            "proxy_command",
            "transfer_protocol",
            "x11",
        ] {
            assert!(
                !json.contains(field),
                "{field} is written only when set: {json}"
            );
        }
        let back: HostConfig = serde_json::from_str(&json).expect("reads");
        assert_eq!(back, host);
    }

    #[test]
    fn the_copy_protocol_is_sftp_unless_the_host_says_otherwise() {
        let mut host = host();
        assert_eq!(
            host.transfer_protocol(),
            TransferProtocol::Sftp,
            "a host saved before the choice existed copies over SFTP"
        );
        assert!(
            !serde_json::to_string(&host)
                .expect("serializes")
                .contains("transfer_protocol"),
            "the default is not written as a field"
        );

        host.set_transfer_protocol(TransferProtocol::Scp);
        let json = serde_json::to_string(&host).expect("serializes");
        assert!(json.contains("\"transfer_protocol\":\"Scp\""), "{json}");
        let back: HostConfig = serde_json::from_str(&json).expect("reads");
        assert_eq!(back.transfer_protocol(), TransferProtocol::Scp);

        // Choosing the default again is the absence of a choice, not a second encoding
        // of it: the record shrinks back to what an older build wrote.
        host.set_transfer_protocol(TransferProtocol::Sftp);
        assert!(
            !serde_json::to_string(&host)
                .expect("serializes")
                .contains("transfer_protocol"),
            "the default is stored as nothing at all"
        );
    }

    #[test]
    fn the_sign_in_choice_and_the_command_round_trip() {
        let mut host = host();
        host.set_auth(Some(AuthMethodKind::Password));
        host.set_remember_secret(true);
        host.set_startup_command(Some("  cd /srv\n tmux attach  "));
        let json = serde_json::to_string(&host).expect("serializes");
        assert!(json.contains("\"auth\":\"password\""), "{json}");
        let back: HostConfig = serde_json::from_str(&json).expect("reads");
        assert_eq!(back.auth(), Some(AuthMethodKind::Password));
        assert!(back.remembers_secret());
        assert_eq!(back.startup_command(), Some("cd /srv; tmux attach"));
    }

    #[test]
    fn only_a_method_with_a_secret_can_remember_one() {
        let mut host = host();
        host.set_remember_secret(true);
        assert!(
            !host.remembers_secret(),
            "no method chosen, nothing to remember"
        );
        host.set_auth(Some(AuthMethodKind::Password));
        host.set_remember_secret(true);
        assert!(host.remembers_secret());
        host.set_auth(Some(AuthMethodKind::Agent));
        assert!(
            !host.remembers_secret(),
            "switching to the agent drops the flag"
        );
        host.set_startup_command(Some("   "));
        assert_eq!(host.startup_command(), None);
    }

    #[test]
    fn x11_forwarding_is_off_unless_the_host_was_told_otherwise() {
        let mut host = host();
        assert_eq!(
            host.x11(),
            X11Forwarding::Off,
            "a host saved before the choice existed forwards no display"
        );
        assert_eq!(X11Forwarding::default(), X11Forwarding::Off);
        assert!(
            !serde_json::to_string(&host)
                .expect("serializes")
                .contains("\"x11\""),
            "the off default is not written as a field"
        );
        assert!(!X11Forwarding::Off.is_enabled());
        assert!(!X11Forwarding::Off.needs_risk_confirmation());

        host.set_x11(X11Forwarding::Restricted);
        let json = serde_json::to_string(&host).expect("serializes");
        assert!(json.contains("\"x11\":\"Restricted\""), "{json}");
        let back: HostConfig = serde_json::from_str(&json).expect("reads");
        assert_eq!(back.x11(), X11Forwarding::Restricted);
        assert!(X11Forwarding::Restricted.is_enabled());
        assert!(
            !X11Forwarding::Restricted.needs_risk_confirmation(),
            "the restricted mode is the one that needs no risk prompt"
        );

        host.set_x11(X11Forwarding::Trusted);
        let back: HostConfig =
            serde_json::from_str(&serde_json::to_string(&host).expect("serializes"))
                .expect("reads");
        assert_eq!(back.x11(), X11Forwarding::Trusted);
        assert!(X11Forwarding::Trusted.needs_risk_confirmation());

        // Choosing the default again is the absence of a choice, not a second
        // encoding of it: the record shrinks back to what an older build wrote.
        host.set_x11(X11Forwarding::Off);
        assert!(
            !serde_json::to_string(&host)
                .expect("serializes")
                .contains("\"x11\""),
            "the default is stored as nothing at all"
        );
    }

    #[test]
    fn every_x11_mode_is_a_distinct_value_a_chooser_can_show() {
        let rendered: Vec<String> = X11Forwarding::ALL.iter().map(ToString::to_string).collect();
        assert_eq!(rendered, ["off", "restricted", "trusted"]);
        assert_eq!(X11Forwarding::ALL.len(), 3);
    }

    #[test]
    fn a_blank_label_keeps_the_group_path_and_only_a_path_clears_the_label() {
        let mut host = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("valid host");
        let nested = GroupPath::parse("Prod/Web").expect("a valid path");
        host.set_group_path(Some(nested.clone()));
        assert_eq!(host.group_path(), Some(nested.clone()));
        assert_eq!(host.group(), None, "a hierarchy has no label to file it by");

        // The host form saves its group box on every edit, and for a nested host that
        // box is empty: an unrelated edit must not move the host out of its group.
        host.set_group(Some("   "));
        assert_eq!(
            host.group_path(),
            Some(nested.clone()),
            "a blank label is not a request to leave the group"
        );

        // A real label still replaces the hierarchy, so the two fields never disagree.
        host.set_group(Some(" Lab "));
        assert_eq!(host.group(), Some("Lab"));
        assert_eq!(
            host.group_path(),
            Some(GroupPath::parse("Lab").expect("a valid path")),
            "the label is the whole path again, not the old hierarchy"
        );

        // Leaving every group for real clears a legacy label as well.
        host.set_group_path(Some(nested));
        host.set_group(Some("Legacy"));
        host.set_group_path(None);
        assert_eq!(host.group(), None, "the old label is cleared");
        assert_eq!(host.group_path(), None);
    }

    #[test]
    fn a_host_that_states_no_connection_values_inherits_and_says_so_on_disk() {
        let host = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("valid");
        assert_eq!(host.connection(), None);
        let json = serde_json::to_string(&host).expect("serializes");
        assert!(
            !json.contains("connection"),
            "an older record grows no field: {json}"
        );
        let defaults = ConnectionDefaults::default();
        assert_eq!(
            host.connection_policy(defaults),
            defaults.policy(),
            "a host that says nothing is dialled with the global defaults"
        );
    }

    #[test]
    fn a_host_override_and_an_explicit_off_survive_a_save() {
        let defaults = ConnectionDefaults::default();
        let mut host = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("valid");
        let own = HostConnection::new(Some(120), Some(5), Some(9)).expect("valid");
        host.set_connection(Some(own));
        let json = serde_json::to_string(&host).expect("serializes");
        assert!(json.contains("\"keepalive_interval_secs\":120"), "{json}");
        let back: HostConfig = serde_json::from_str(&json).expect("reads");
        assert_eq!(back.connection(), Some(own));
        let policy = back.connection_policy(defaults);
        assert_eq!(
            policy.keepalive_interval(),
            Some(std::time::Duration::from_secs(120))
        );
        assert_eq!(policy.keepalive_max(), 5);
        assert_eq!(policy.connect_timeout(), std::time::Duration::from_secs(9));

        // An explicit off is a choice, not the absence of one, and it is not the
        // global default; changing the default must not switch this host back on.
        let mut off = host.clone();
        off.set_connection(Some(HostConnection::keepalive_off()));
        assert!(off.connection_policy(defaults).keepalive().is_none());
        let other = ConnectionDefaults::new(90, 6, 45).expect("valid");
        assert!(
            off.connection_policy(other).keepalive().is_none(),
            "an explicit off outlives a change to the global default"
        );
        assert_eq!(
            off.connection_policy(other).connect_timeout(),
            std::time::Duration::from_secs(45),
            "the fields the host did not state still inherit"
        );

        // Clearing the last value is inheritance again, and the record shrinks.
        let mut cleared = host.clone();
        cleared.set_connection(Some(HostConnection::default()));
        assert_eq!(cleared.connection(), None);
        assert!(
            !serde_json::to_string(&cleared)
                .expect("serializes")
                .contains("connection")
        );
        cleared.set_connection(None);
        assert_eq!(cleared.connection(), None);
    }

    #[test]
    fn a_stored_connection_value_that_cannot_be_used_fails_the_read() {
        let host = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("valid");
        let json = serde_json::to_string(&host).expect("serializes");
        let broken = json.replacen('{', "{\"connection\":{\"keepalive_max_misses\":0},", 1);
        let error = serde_json::from_str::<HostConfig>(&broken)
            .expect_err("a silent-connection count of 0 is not applied");
        assert!(
            error.to_string().contains('0'),
            "the failure names the value: {error}"
        );
        assert!(
            serde_json::from_str::<HostConfig>(&broken.replace(
                "\"keepalive_max_misses\":0",
                "\"keepalive_interval_secs\":0"
            ))
            .is_ok(),
            "an interval of 0 is a valid explicit off"
        );
    }
}

#[cfg(test)]
mod jump_tests {
    use super::*;

    fn named(name: &str) -> HostConfig {
        HostConfig::new(name, "10.0.0.5", 22, "deploy").expect("valid")
    }

    #[test]
    fn a_jump_chain_and_proxy_command_round_trip_without_a_secret_field() {
        let bastion = named("bastion");
        let mut target = named("web");
        let secret = "hunter2-not-a-field";
        target.set_jumps([bastion.id()]);
        target.set_proxy_command(Some("  ssh -W %h:%p bastion  "));
        target.set_proxy_command(Some("   "));
        assert_eq!(target.proxy_command(), None, "a blank command clears it");
        target.set_proxy_command(Some(" ssh -W %h:%p bastion "));

        let json = serde_json::to_string(&target).expect("serializes");
        assert!(!json.contains(secret), "{json}");
        assert!(!json.contains("password"), "{json}");
        let back: HostConfig = serde_json::from_str(&json).expect("reads");
        assert_eq!(back.jumps(), &[bastion.id()]);
        assert_eq!(back.proxy_command(), Some("ssh -W %h:%p bastion"));
        assert_eq!(back, target);
    }

    #[test]
    fn a_cycle_and_a_missing_hop_are_local_and_leave_other_hosts_alone() {
        let mut gateway = named("gateway");
        let mut bastion = named("bastion");
        let mut web = named("web");
        let lab = named("lab");
        let missing = HostId::new();
        gateway.set_jumps([bastion.id()]);
        bastion.set_jumps([gateway.id()]);
        web.set_jumps([lab.id(), missing]);

        let hosts = [gateway.clone(), bastion.clone(), web.clone(), lab.clone()];
        let faults = jump_faults(&hosts);
        assert!(
            faults.iter().any(|fault| matches!(
                fault,
                JumpFault::Cycle { path, .. } if path.first() == Some(&gateway.id())
                    && path.contains(&bastion.id())
            )),
            "the loop is named, got {faults:?}"
        );
        assert!(
            faults.iter().any(|fault| matches!(
                fault,
                JumpFault::MissingHop { host, index: 1, missing: id, .. }
                    if *host == web.id() && *id == missing
            )),
            "only web's second hop is missing, got {faults:?}"
        );
        assert!(
            faults
                .iter()
                .all(|fault| !format!("{fault:?}").contains("lab")
                    || matches!(fault, JumpFault::MissingHop { .. })),
            "lab itself is not a broken host"
        );
        let web_fault = connection_jump_fault(&hosts, web.id()).expect("web is blocked");
        assert!(web_fault.message().contains("not saved"));
        assert!(web_fault.message().contains("Remove that hop"));
        assert!(web_fault.message().contains("Nothing was connected"));
        assert!(connection_jump_fault(&hosts, lab.id()).is_none());

        let cycle = connection_jump_fault(&hosts, gateway.id()).expect("gateway loops");
        assert!(cycle.message().contains("loops"));
        assert!(!cycle.message().contains("hunter2"));
        assert!(
            connection_jump_fault(&hosts, HostId::new())
                .expect("unknown")
                .message()
                .contains("Choose a saved host")
        );
    }
}
#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::session::Capability;
    use serde_json::{Value, json};

    fn ssh(name: &str, address: &str) -> HostConfig {
        HostConfig::new(name, address, 22, "deploy").expect("a valid SSH host")
    }

    #[test]
    fn every_protocol_names_itself_the_same_way_in_a_file_and_in_the_list() {
        assert_eq!(ProtocolKind::ALL.len(), 4);
        for protocol in ProtocolKind::ALL {
            let value = serde_json::to_value(protocol).expect("a protocol serializes");
            assert_eq!(
                value,
                Value::String(protocol.wire_name().to_owned()),
                "the stored tag and the reported name have to agree"
            );
            assert_eq!(
                ProtocolKind::from_wire_name(protocol.wire_name()),
                Some(protocol)
            );
        }
        assert_eq!(ProtocolKind::from_wire_name("telnet"), None);
        assert_eq!(
            ProtocolKind::from_wire_name("  RDP "),
            Some(ProtocolKind::Rdp)
        );
        assert_eq!(ProtocolKind::default(), ProtocolKind::Ssh);
    }

    #[test]
    fn a_new_connection_starts_on_the_port_its_protocol_uses() {
        assert_eq!(ProtocolKind::Ssh.default_port().map(Port::get), Some(22));
        assert_eq!(ProtocolKind::Rdp.default_port().map(Port::get), Some(3389));
        assert_eq!(ProtocolKind::Vnc.default_port().map(Port::get), Some(5900));
        assert_eq!(
            ProtocolKind::Serial.default_port(),
            None,
            "a serial device has no TCP port to offer"
        );
    }

    #[test]
    fn an_old_ssh_record_keeps_its_old_shape_and_the_tagged_form_means_the_same() {
        let host = ssh("prod web", "10.0.0.5");
        let stored = serde_json::to_value(&host).expect("the host serializes");
        assert!(
            stored.get("protocol").is_none(),
            "the host shape older builds wrote has no tag: {stored}"
        );
        let legacy: HostConfig =
            serde_json::from_value(stored).expect("the old shape is still a host record");
        let record = ConnectionRecord::from(legacy);
        assert!(record.is_ssh());
        assert_eq!(record.id(), host.id());
        assert_eq!(record.name(), "prod web");
        assert_eq!(record.as_ssh().map(HostConfig::address), Some("10.0.0.5"));

        let tagged = serde_json::to_value(&record).expect("the record serializes");
        assert_eq!(
            tagged["protocol"],
            Value::String("ssh".to_owned()),
            "a connection record always names its protocol"
        );
        let read: ConnectionRecord = serde_json::from_value(tagged).expect("it reads back");
        assert_eq!(read, record);
    }

    #[test]
    fn a_tagged_ssh_record_is_the_same_connection_as_the_untagged_one() {
        let host = ssh("prod web", "10.0.0.5");
        let mut tagged = serde_json::to_value(&host).expect("the host serializes");
        tagged["protocol"] = Value::String("ssh".to_owned());
        let record: ConnectionRecord =
            serde_json::from_value(tagged).expect("an explicit ssh tag names the same protocol");
        assert_eq!(record, ConnectionRecord::Ssh(host));
    }

    #[test]
    fn a_desktop_record_carries_an_explicit_protocol_tag() {
        let mut config = RdpConfig::new("office", "10.0.0.9", Some("alice")).expect("valid");
        config.set_domain(Some("CORP"));
        let record = ConnectionRecord::Rdp(config);
        let written = serde_json::to_value(&record).expect("the record serializes");
        assert_eq!(written["protocol"], json!("rdp"));
        assert_eq!(written["port"], Value::from(3389));
        assert_eq!(written["user"], json!("alice"));
        assert_eq!(written["domain"], json!("CORP"));

        let read: ConnectionRecord = serde_json::from_value(written).expect("it reads back");
        assert_eq!(read, record);
        assert_eq!(read.protocol(), ProtocolKind::Rdp);
        assert!(read.as_ssh().is_none());
    }

    #[test]
    fn an_unknown_protocol_is_refused_instead_of_becoming_a_connection() {
        let text = json!({
            "protocol": "telnet",
            "id": HostId::new(),
            "name": "old switch",
            "address": "10.0.0.2",
            "port": 23
        });
        let error = serde_json::from_value::<ConnectionRecord>(text)
            .expect_err("a protocol this build does not know cannot become a connection");
        assert!(
            error.to_string().contains("telnet"),
            "the refusal names the protocol it found: {error}"
        );
    }

    #[test]
    fn a_field_the_chosen_protocol_does_not_have_is_refused() {
        let text = json!({
            "protocol": "vnc",
            "id": HostId::new(),
            "name": "wall",
            "address": "10.0.0.7",
            "port": 5900,
            "user": "alice"
        });
        let error = serde_json::from_value::<ConnectionRecord>(text)
            .expect_err("VNC has no user, so a record that carries one is not this build's");
        assert!(error.to_string().contains("user"), "{error}");
    }

    #[test]
    fn a_stored_port_outside_the_range_is_refused_rather_than_becoming_dialable() {
        let text = json!({
            "protocol": "rdp",
            "id": HostId::new(),
            "name": "office",
            "address": "10.0.0.9",
            "port": 0
        });
        assert!(serde_json::from_value::<ConnectionRecord>(text).is_err());
    }

    #[test]
    fn the_policies_and_the_echo_flag_survive_a_round_trip() {
        let mut rdp = RdpConfig::new("office", "10.0.0.9", Some("alice")).expect("valid");
        assert_eq!(rdp.certificate(), RdpCertificatePolicy::Ask);
        rdp.set_certificate(RdpCertificatePolicy::Reject);
        let written = serde_json::to_value(ConnectionRecord::Rdp(rdp.clone())).expect("serializes");
        assert_eq!(written["certificate"], Value::String("reject".to_owned()));
        let read: ConnectionRecord = serde_json::from_value(written).expect("reads back");
        assert_eq!(read, ConnectionRecord::Rdp(rdp));

        // The default policy is left out, so a record that never chose one keeps the
        // shape an older build wrote.
        let plain = RdpConfig::new("office", "10.0.0.9", None::<String>).expect("valid");
        let written = serde_json::to_value(ConnectionRecord::Rdp(plain)).expect("serializes");
        assert!(written.get("certificate").is_none(), "{written}");

        let mut vnc = VncConfig::new("wall", "10.0.0.7").expect("valid");
        assert_eq!(vnc.security(), VncSecurity::Automatic);
        vnc.set_security(VncSecurity::EncryptedOnly);
        let written = serde_json::to_value(ConnectionRecord::Vnc(vnc.clone())).expect("serializes");
        assert_eq!(
            written["security"],
            Value::String("encrypted_only".to_owned())
        );
        let read: ConnectionRecord = serde_json::from_value(written).expect("reads back");
        assert_eq!(read, ConnectionRecord::Vnc(vnc));

        let mut console = SerialConfig::new("console", "COM3").expect("valid");
        assert!(!console.local_echo());
        console.set_local_echo(true);
        let written =
            serde_json::to_value(ConnectionRecord::Serial(console.clone())).expect("serializes");
        assert_eq!(written["local_echo"], Value::Bool(true));
        let read: ConnectionRecord = serde_json::from_value(written).expect("reads back");
        assert_eq!(read, ConnectionRecord::Serial(console));
    }
    #[test]
    fn the_clipboard_is_shared_by_default_and_only_turning_it_off_is_written() {
        let rdp = RdpConfig::new("office", "10.0.0.9", None::<String>).expect("valid");
        assert!(rdp.clipboard(), "a new RDP connection shares the clipboard");
        assert_eq!(rdp.drive_folder(), None, "and maps no folder");
        let vnc = VncConfig::new("wall", "10.0.0.7").expect("valid");
        assert!(vnc.clipboard(), "a new VNC connection shares the clipboard");

        // The default stays out of the file, so the record keeps the shape an older
        // build reads.
        for record in [
            ConnectionRecord::Rdp(rdp.clone()),
            ConnectionRecord::Vnc(vnc.clone()),
        ] {
            let written = serde_json::to_value(&record).expect("serializes");
            assert!(written.get("clipboard").is_none(), "{written}");
            assert!(written.get("drive_folder").is_none(), "{written}");
        }

        let mut rdp = rdp;
        rdp.set_clipboard(false);
        rdp.set_drive_folder(Some("  C:/Users/alice/Shared  "))
            .expect("a usable folder");
        assert_eq!(rdp.drive_folder(), Some("C:/Users/alice/Shared"));
        let written = serde_json::to_value(ConnectionRecord::Rdp(rdp.clone())).expect("serializes");
        assert_eq!(written["clipboard"], Value::Bool(false));
        assert_eq!(written["drive_folder"], json!("C:/Users/alice/Shared"));
        let read: ConnectionRecord = serde_json::from_value(written).expect("reads back");
        assert_eq!(read, ConnectionRecord::Rdp(rdp));

        let mut vnc = vnc;
        vnc.set_clipboard(false);
        let written = serde_json::to_value(ConnectionRecord::Vnc(vnc.clone())).expect("serializes");
        assert_eq!(written["clipboard"], Value::Bool(false));
        let read: ConnectionRecord = serde_json::from_value(written).expect("reads back");
        assert_eq!(read, ConnectionRecord::Vnc(vnc));
    }

    #[test]
    fn a_record_written_before_the_clipboard_choice_reads_as_sharing_it() {
        let rdp: ConnectionRecord = serde_json::from_value(json!({
            "protocol": "rdp",
            "id": HostId::new(),
            "name": "office",
            "address": "10.0.0.9",
            "port": 3389
        }))
        .expect("an older record still reads");
        let ConnectionRecord::Rdp(rdp) = rdp else {
            panic!("an RDP record");
        };
        assert!(rdp.clipboard());
        assert_eq!(rdp.drive_folder(), None);

        let vnc: ConnectionRecord = serde_json::from_value(json!({
            "protocol": "vnc",
            "id": HostId::new(),
            "name": "wall",
            "address": "10.0.0.7",
            "port": 5900
        }))
        .expect("an older record still reads");
        let ConnectionRecord::Vnc(vnc) = vnc else {
            panic!("a VNC record");
        };
        assert!(vnc.clipboard());
    }

    #[test]
    fn a_mapped_folder_is_trimmed_and_a_control_character_is_refused() {
        let mut rdp = RdpConfig::new("office", "10.0.0.9", None::<String>).expect("valid");
        rdp.set_drive_folder(Some("/home/alice/shared"))
            .expect("a usable folder");
        assert_eq!(
            rdp.set_drive_folder(Some("/home/alice/\u{7}bell")),
            Err(ConnectionValidationError::DriveFolderContainsControlCharacter)
        );
        assert_eq!(
            rdp.drive_folder(),
            Some("/home/alice/shared"),
            "a refused folder leaves the previous one in place"
        );
        rdp.set_drive_folder(Some("   "))
            .expect("blank turns it off");
        assert_eq!(rdp.drive_folder(), None);
        rdp.set_drive_folder(Some("/srv")).expect("usable");
        rdp.set_drive_folder(None::<String>)
            .expect("none turns it off");
        assert_eq!(rdp.drive_folder(), None);

        // A stored blank folder is no folder; a stored control character is not a
        // record this build wrote.
        let base = json!({
            "protocol": "rdp",
            "id": HostId::new(),
            "name": "office",
            "address": "10.0.0.9",
            "port": 3389
        });
        let mut blank = base.clone();
        blank["drive_folder"] = json!("  ");
        let read: ConnectionRecord = serde_json::from_value(blank).expect("reads");
        let ConnectionRecord::Rdp(read) = read else {
            panic!("an RDP record");
        };
        assert_eq!(read.drive_folder(), None);
        let mut broken = base;
        broken["drive_folder"] = json!("C:/a\nb");
        assert!(serde_json::from_value::<ConnectionRecord>(broken).is_err());

        // VNC has no folder to map.
        let vnc = json!({
            "protocol": "vnc",
            "id": HostId::new(),
            "name": "wall",
            "address": "10.0.0.7",
            "port": 5900,
            "drive_folder": "/srv"
        });
        assert!(serde_json::from_value::<ConnectionRecord>(vnc).is_err());
    }

    #[test]
    fn a_copy_keeps_the_clipboard_choice_and_the_mapped_folder() {
        let mut rdp = RdpConfig::new("office", "10.0.0.9", None::<String>).expect("valid");
        rdp.set_clipboard(false);
        rdp.set_drive_folder(Some("D:/exchange")).expect("usable");
        rdp.set_remember_secret(true);
        let original = ConnectionRecord::Rdp(rdp);
        let ConnectionRecord::Rdp(copy) = original.duplicated() else {
            panic!("still RDP");
        };
        assert_ne!(copy.id(), original.id());
        assert!(!copy.clipboard());
        assert_eq!(copy.drive_folder(), Some("D:/exchange"));
        assert!(!copy.remembers_secret());

        let mut vnc = VncConfig::new("wall", "10.0.0.7").expect("valid");
        vnc.set_clipboard(false);
        let ConnectionRecord::Vnc(copy) = ConnectionRecord::Vnc(vnc).duplicated() else {
            panic!("still VNC");
        };
        assert!(!copy.clipboard());
    }

    #[test]
    fn a_serial_record_checks_the_device_and_the_frame() {
        let mut config = SerialConfig::new("console", "COM3").expect("a valid device");
        assert_eq!(config.baud(), DEFAULT_SERIAL_BAUD);
        assert_eq!(config.data_bits(), MAX_SERIAL_DATA_BITS);
        assert_eq!(config.frame(), "8N1");
        assert_eq!(config.target(), "COM3@115200 8N1");
        assert_eq!(config.flow(), SerialFlow::None);

        config.set_baud(9_600).expect("a common speed");
        config.set_data_bits(7).expect("a usable width");
        config.set_parity(SerialParity::Even);
        config.set_stop_bits(SerialStopBits::Two);
        config.set_flow(SerialFlow::Hardware);
        assert_eq!(config.frame(), "7E2");

        assert_eq!(
            SerialConfig::new("  ", "COM3"),
            Err(ConnectionValidationError::EmptyName)
        );
        assert_eq!(
            SerialConfig::new("console", "   "),
            Err(ConnectionValidationError::EmptyDevice)
        );
        assert_eq!(
            SerialConfig::new("console", "COM\u{7}3"),
            Err(ConnectionValidationError::DeviceContainsControlCharacter)
        );
        assert_eq!(
            config.set_baud(10),
            Err(ConnectionValidationError::BaudOutOfRange { value: 10 })
        );
        assert_eq!(
            config.set_data_bits(9),
            Err(ConnectionValidationError::DataBitsOutOfRange { value: 9 })
        );
        assert!(!ConnectionRecord::Serial(config).remembers_secret());
    }

    #[test]
    fn the_capability_set_is_what_each_protocol_can_really_do() {
        let ssh = ConnectionRecord::Ssh(ssh("web", "10.0.0.5")).capabilities();
        for capability in [
            Capability::Terminal,
            Capability::Files,
            Capability::Batch,
            Capability::Forwarding,
            Capability::X11,
            Capability::Clipboard,
        ] {
            assert!(ssh.allows(capability), "SSH offers {capability:?}");
        }
        assert!(
            !ssh.allows(Capability::Desktop),
            "an SSH session is a character grid, not a framebuffer"
        );

        let rdp = ConnectionRecord::Rdp(
            RdpConfig::new("office", "10.0.0.9", None::<String>).expect("valid"),
        )
        .capabilities();
        assert!(rdp.allows(Capability::Desktop));
        assert!(rdp.allows(Capability::Clipboard));
        for withheld in [
            Capability::Terminal,
            Capability::Files,
            Capability::Batch,
            Capability::Forwarding,
            Capability::X11,
        ] {
            assert!(
                !rdp.allows(withheld),
                "a desktop does not offer {withheld:?}"
            );
        }

        let vnc = ConnectionRecord::Vnc(VncConfig::new("wall", "10.0.0.7").expect("valid"))
            .capabilities();
        assert_eq!(
            vnc.iter().collect::<Vec<_>>(),
            vec![Capability::Desktop, Capability::Clipboard]
        );

        let serial = ConnectionRecord::Serial(SerialConfig::new("console", "COM3").expect("valid"))
            .capabilities();
        assert_eq!(
            serial.iter().collect::<Vec<_>>(),
            vec![Capability::Terminal, Capability::Clipboard]
        );
        assert!(!serial.is_empty());
    }

    #[test]
    fn only_ssh_carries_the_batch_forwarding_and_file_entry_points() {
        for protocol in [ProtocolKind::Rdp, ProtocolKind::Vnc, ProtocolKind::Serial] {
            let capabilities = SessionCapabilities::of_protocol(protocol);
            assert!(!capabilities.allows(Capability::Batch));
            assert!(!capabilities.allows(Capability::Forwarding));
            assert!(!capabilities.allows(Capability::X11));
            assert!(!capabilities.allows(Capability::Files));
        }
        assert!(SessionCapabilities::of_protocol(ProtocolKind::Ssh).allows(Capability::Files));
    }

    #[test]
    fn a_copy_gets_a_new_identity_and_inherits_no_credential_or_history() {
        let mut original = ssh("prod web", "10.0.0.5");
        original.set_remember_secret(true);
        let original = ConnectionRecord::Ssh(original);
        let copy = original.duplicated();
        assert_ne!(copy.id(), original.id(), "a copy is a new connection");
        assert_eq!(copy.name(), original.name());
        assert!(
            !copy.remembers_secret(),
            "the copy has an identifier the credential store has never seen"
        );
        assert_eq!(copy.target(), original.target());

        let mut jumper = ssh("gateway", "10.0.0.1");
        jumper.set_jumps([HostId::new()]);
        let copy = ConnectionRecord::Ssh(jumper).duplicated();
        let host = copy.as_ssh().expect("still SSH");
        assert_eq!(host.jumps().len(), 1);
        assert!(
            !host.jumps().contains(&host.id()),
            "a duplicate never names itself in its own jump chain"
        );
    }

    #[test]
    fn a_connection_is_found_by_its_name_its_protocol_or_its_target() {
        let record = ConnectionRecord::Rdp(
            RdpConfig::new("Office desktop", "10.0.0.9", Some("alice")).expect("valid"),
        );
        assert!(record.matches("office"));
        assert!(record.matches("rdp"));
        assert!(record.matches("RDP"));
        assert!(record.matches("10.0.0.9:3389"));
        assert!(record.matches("alice@10.0.0.9"));
        assert!(
            record.matches("   "),
            "an empty phrase keeps every connection"
        );
        assert!(!record.matches("vnc"));
        assert!(!record.matches("warehouse"));
    }

    #[test]
    fn both_group_spellings_are_read_the_way_the_ssh_record_reads_them() {
        let mut config = VncConfig::new("wall", "10.0.0.7").expect("valid");
        config.set_group(Some("Prod"));
        assert_eq!(config.group(), Some("Prod"));
        let path = GroupPath::parse("Prod/East").expect("a usable path");
        config.set_group_path(Some(path.clone()));
        assert_eq!(config.group(), None, "one record carries one spelling");
        assert_eq!(config.group_path(), Some(&path));
        let record = ConnectionRecord::Vnc(config);
        assert_eq!(record.group_text(), Some("Prod/East".to_owned()));

        let written = serde_json::to_value(&record).expect("serializes");
        assert_eq!(written["group_path"], Value::String("Prod/East".to_owned()));
        assert!(written.get("group").is_none());
        let read: ConnectionRecord = serde_json::from_value(written).expect("reads back");
        assert_eq!(read.group_path(), Some(path));
    }
}
