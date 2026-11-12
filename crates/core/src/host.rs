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

use crate::group::GroupPath;
use crate::ids::HostId;
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
    /// Naming a label replaces any hierarchy this host was in: a host belongs to one
    /// place, and two fields that could disagree would make the group counts wrong.
    pub fn set_group(&mut self, group: Option<impl Into<String>>) {
        self.group = group
            .map(Into::into)
            .map(|group: String| group.trim().to_owned())
            .filter(|group| !group.is_empty());
        if self.group.is_some() {
            self.group_path = None;
        }
    }

    /// Moves this host into a group path, or out of every group.
    ///
    /// The path was validated when it was built, so this cannot fail. Assigning a path
    /// clears a legacy label for the same reason [`Self::set_group`] clears a path.
    pub fn set_group_path(&mut self, path: Option<GroupPath>) {
        if path.is_some() {
            self.group = None;
        }
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
