//! Host management state: the editor draft, the three list views and the messages.
//!
//! This module is render-independent on purpose. The validation, the search, the
//! grouping and the recency ordering are all decided here, in plain Rust, so they
//! can be tested without a window; `crate::app::view` only draws what this module
//! reports.
//!
//! Two rules shape the state:
//!
//! * a draft is text until it is saved, so an invalid port is refused by the domain
//!   constructor and the editor keeps what the user typed;
//! * a message says what really happened. "Saved" never implies a connection was
//!   opened, and a failure keeps the reason the storage layer produced.

//!
//! The list itself is a row model, not a set of elements: a group header and the hosts
//! the current filters keep are flattened into rows that carry a stable HostId or
//! GroupPath, so five thousand saved hosts cost five thousand rows of data and only the
//! visible ones are drawn (R18, ADR-8). Nothing here dials: the connection state a filter
//! reads is the state the window already holds.

use std::collections::BTreeSet;

use shellune_core::{
    AuthMethodKind, ConnectionDefaults, ConnectionValueError, DEFAULT_KEEPALIVE_INTERVAL_SECS,
    GroupBook, GroupPath, HostConfig, HostConnection, HostId, MAX_CONNECT_TIMEOUT_SECS,
    MAX_KEEPALIVE_INTERVAL_SECS, MAX_KEEPALIVE_MAX_MISSES, Port, TransferProtocol, X11Forwarding,
};
use shellune_storage::{
    DEFAULT_CONFIG_TEXT, HostBook, HostEdit, ImportOutcome, ImportPlan, ImportPreview,
    ImportedHost, host_matches,
};

use crate::app::copy::{Strings, UxStrings};

/// Which subset of the saved hosts the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostView {
    /// Every saved host, in saved order.
    #[default]
    All,
    /// The same hosts, grouped by group label.
    Groups,
    /// The hosts that were really reached, newest first.
    Recent,
}

impl HostView {
    /// Every view, in the order the switch shows them.
    pub const ALL: [Self; 3] = [Self::All, Self::Groups, Self::Recent];

    /// Label of the view switch entry.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::All => strings.hosts_view_all,
            Self::Groups => strings.hosts_view_groups,
            Self::Recent => strings.hosts_view_recent,
        }
    }

    /// Debug selector of the view switch entry, used by the layout tests.
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::All => "hosts.view.all",
            Self::Groups => "hosts.view.groups",
            Self::Recent => "hosts.view.recent",
        }
    }
}

/// One field of the host form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostField {
    /// The display name.
    Name,
    /// The address the next connection dials.
    Address,
    /// The TCP port.
    Port,
    /// The login user.
    User,
    /// The optional group label.
    Group,
    /// The optional private key path a connection may use.
    ///
    /// The editor only ever holds a path: Shellune never reads the key file, so there
    /// is nothing here that could hold a key or its passphrase.
    IdentityFile,
    /// The password or key passphrase, typed to be remembered.
    ///
    /// The draft holds it only while the form is open; it is handed to the system
    /// credential store on save and never reaches the host record.
    Secret,
    /// The command typed into the remote shell after connecting.
    StartupCommand,
    /// The ProxyCommand template. Blank means none, and nothing is run from the form.
    ProxyCommand,
}

impl HostField {
    /// Every field, in form order.
    pub const ALL: [Self; 9] = [
        Self::Name,
        Self::Address,
        Self::Port,
        Self::User,
        Self::Group,
        Self::IdentityFile,
        Self::Secret,
        Self::StartupCommand,
        Self::ProxyCommand,
    ];

    /// Label shown in front of the field.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::Name => strings.host_field_name,
            Self::Address => strings.host_field_address,
            Self::Port => strings.host_field_port,
            Self::User => strings.host_field_user,
            Self::Group => strings.host_field_group,
            Self::IdentityFile => strings.host_field_identity,
            Self::Secret => strings.auth_password,
            Self::StartupCommand => strings.host_startup_label,
            Self::ProxyCommand => strings.host_proxy_label,
        }
    }

    /// Debug selector of the field, used by the tests.
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::Name => "hosts.field.name",
            Self::Address => "hosts.field.address",
            Self::Port => "hosts.field.port",
            Self::User => "hosts.field.user",
            Self::Group => "hosts.field.group",
            Self::IdentityFile => "hosts.field.identity",
            Self::Secret => "hosts.field.secret",
            Self::StartupCommand => "hosts.field.startup",
            Self::ProxyCommand => "hosts.field.proxy",
        }
    }
}

/// One of the three numbers a connection policy is made of (R20).
///
/// The name exists so a refusal can point at the field that produced it: a page that
/// silently corrected a number would leave the user with a value they did not choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionField {
    /// How long a silent connection waits before one probe.
    KeepaliveInterval,
    /// How many probes may go unanswered before the link counts as dead.
    KeepaliveMisses,
    /// How long the machine part of an attempt may take.
    ConnectTimeout,
}

impl ConnectionField {
    /// The three fields, in the order a page lists them.
    pub const ALL: [Self; 3] = [
        Self::KeepaliveInterval,
        Self::KeepaliveMisses,
        Self::ConnectTimeout,
    ];

    /// Stable identifier, used by the selectors and the capture scenarios.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::KeepaliveInterval => "keepalive-interval",
            Self::KeepaliveMisses => "keepalive-misses",
            Self::ConnectTimeout => "connect-timeout",
        }
    }

    /// The name of the field in one language.
    #[must_use]
    pub fn label(self, ux: &UxStrings) -> &'static str {
        match self {
            Self::KeepaliveInterval => ux.settings_keepalive_interval,
            Self::KeepaliveMisses => ux.settings_keepalive_misses,
            Self::ConnectTimeout => ux.settings_connect_timeout,
        }
    }

    /// The field a refused value belongs to.
    #[must_use]
    pub fn of_error(error: &ConnectionValueError) -> Self {
        match error {
            ConnectionValueError::KeepaliveIntervalOutOfRange { .. } => Self::KeepaliveInterval,
            ConnectionValueError::KeepaliveMaxMissesOutOfRange { .. } => Self::KeepaliveMisses,
            ConnectionValueError::ConnectTimeoutOutOfRange { .. } => Self::ConnectTimeout,
        }
    }
}

/// Why a typed connection number was refused.
///
/// The text stays in the field: the reason is shown beside it and the value in force
/// is unchanged, which is the difference between refusing a number and quietly fixing
/// it (R20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionInputError {
    /// The field is empty.
    Missing,
    /// The text is not a whole number.
    NotANumber {
        /// What was typed.
        text: String,
    },
    /// The text is below zero.
    Negative {
        /// What was typed.
        text: String,
    },
    /// The number is outside what the SSH layer stores.
    Refused(ConnectionValueError),
}

/// The sentence for a refused connection value, in a language.
///
/// The domain crate words its refusals in English so they stay language-neutral; a page
/// says them in the language of the window, with the limit filled in.
#[must_use]
pub fn connection_problem_text(
    problem: &ConnectionInputError,
    locale: crate::app::copy::Locale,
) -> String {
    let ux = crate::app::copy::ux_for(locale);
    let fill = crate::app::copy::fill;
    match problem {
        ConnectionInputError::Missing => ux.connection_problem_missing.to_owned(),
        ConnectionInputError::NotANumber { text } => {
            fill(ux.connection_problem_number, &[("text", text)])
        }
        ConnectionInputError::Negative { text } => {
            fill(ux.connection_problem_negative, &[("text", text)])
        }
        ConnectionInputError::Refused(error) => match error {
            ConnectionValueError::KeepaliveIntervalOutOfRange { value, max } => fill(
                ux.connection_problem_interval,
                &[("value", &value.to_string()), ("max", &max.to_string())],
            ),
            ConnectionValueError::KeepaliveMaxMissesOutOfRange { value, max } => fill(
                ux.connection_problem_misses,
                &[("value", &value.to_string()), ("max", &max.to_string())],
            ),
            ConnectionValueError::ConnectTimeoutOutOfRange { value, max } => fill(
                ux.connection_problem_timeout,
                &[("value", &value.to_string()), ("max", &max.to_string())],
            ),
        },
    }
}

/// The refusal a field reports when a number is beyond every value this build stores.
fn beyond_any_value(field: ConnectionField) -> ConnectionValueError {
    match field {
        ConnectionField::KeepaliveInterval => ConnectionValueError::KeepaliveIntervalOutOfRange {
            value: u32::MAX,
            max: MAX_KEEPALIVE_INTERVAL_SECS,
        },
        ConnectionField::KeepaliveMisses => ConnectionValueError::KeepaliveMaxMissesOutOfRange {
            value: u32::MAX,
            max: MAX_KEEPALIVE_MAX_MISSES,
        },
        ConnectionField::ConnectTimeout => ConnectionValueError::ConnectTimeoutOutOfRange {
            value: u32::MAX,
            max: MAX_CONNECT_TIMEOUT_SECS,
        },
    }
}

/// Asks the domain whether one number is usable, without storing it.
///
/// The check is the domain's own: the setters on a host connection record validate one
/// field at a time and use exactly the bounds the saved records are read with, so a page
/// cannot accept a number a restart would refuse.
fn check_connection_value(field: ConnectionField, value: u32) -> Result<(), ConnectionInputError> {
    let mut record = HostConnection::default();
    let checked = match field {
        ConnectionField::KeepaliveInterval => record.set_keepalive_interval_secs(Some(value)),
        ConnectionField::KeepaliveMisses => record.set_keepalive_max_misses(Some(value)),
        ConnectionField::ConnectTimeout => record.set_connect_timeout_secs(Some(value)),
    };
    checked.map_err(ConnectionInputError::Refused)
}

/// Reads one field of the connection page.
///
/// Zero is a real value for the interval - OpenSSH's way of switching the probe off - and
/// is therefore not refused. A probe count or a timeout of zero is refused, because
/// neither has a meaning that could be applied.
fn parse_connection_number(
    field: ConnectionField,
    text: &str,
) -> Result<u32, ConnectionInputError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ConnectionInputError::Missing);
    }
    if text.starts_with('-') {
        return Err(ConnectionInputError::Negative {
            text: text.to_owned(),
        });
    }
    let Ok(digits) = text.parse::<u64>() else {
        return Err(ConnectionInputError::NotANumber {
            text: text.to_owned(),
        });
    };
    if digits > u64::from(u32::MAX) {
        return Err(ConnectionInputError::Refused(beyond_any_value(field)));
    }
    let value = u32::try_from(digits).unwrap_or(u32::MAX);
    check_connection_value(field, value)?;
    Ok(value)
}

/// How many seconds may pass without a reply before the connection counts as lost.
///
/// It is the interval times the probes: OpenSSH sends one probe per interval and gives up
/// after that many unanswered ones, which is what "dropped after 3 unanswered probes"
/// means in seconds (R20).
#[must_use]
pub fn detection_window_secs(interval: u32, misses: u32) -> u64 {
    u64::from(interval) * u64::from(misses)
}

/// The global keepalive and timeout numbers, as the connection page edits them (R20).
///
/// The numbers stay text while they are being typed and the page keeps the reason the
/// last one was refused, so an unreadable value is visible in the field that produced it
/// rather than replaced by a nearby one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionDraft {
    interval: String,
    misses: String,
    timeout: String,
    /// The interval to put back when keepalive is switched on after being off.
    last_interval: u32,
    error: Option<(ConnectionField, ConnectionInputError)>,
}

impl ConnectionDraft {
    /// The page's copy of the defaults in force.
    #[must_use]
    pub fn of(defaults: ConnectionDefaults) -> Self {
        let mut draft = Self {
            interval: defaults.keepalive_interval_secs().to_string(),
            misses: defaults.keepalive_max_misses().to_string(),
            timeout: defaults.connect_timeout_secs().to_string(),
            last_interval: DEFAULT_KEEPALIVE_INTERVAL_SECS,
            error: None,
        };
        draft.remember_interval();
        draft
    }

    /// The text of one field.
    #[must_use]
    pub fn text(&self, field: ConnectionField) -> &str {
        match field {
            ConnectionField::KeepaliveInterval => &self.interval,
            ConnectionField::KeepaliveMisses => &self.misses,
            ConnectionField::ConnectTimeout => &self.timeout,
        }
    }

    /// What the last edit refused, and which field it was.
    #[must_use]
    pub fn error(&self) -> Option<(ConnectionField, &ConnectionInputError)> {
        self.error
            .as_ref()
            .map(|(field, problem)| (*field, problem))
    }

    /// Replaces one field, keeping the reason if the value is unusable.
    ///
    /// Returns whether the text changed. Every field is re-read after an edit, so the
    /// reason names the field the user has to fix and goes away when it is usable.
    pub fn set_text(&mut self, field: ConnectionField, text: impl Into<String>) -> bool {
        let text = text.into();
        let slot = match field {
            ConnectionField::KeepaliveInterval => &mut self.interval,
            ConnectionField::KeepaliveMisses => &mut self.misses,
            ConnectionField::ConnectTimeout => &mut self.timeout,
        };
        if *slot == text {
            return false;
        }
        *slot = text;
        self.revalidate();
        self.remember_interval();
        true
    }

    /// Re-reads every field and keeps the first refusal, so no message outlives its fix.
    fn revalidate(&mut self) {
        self.error = ConnectionField::ALL.iter().find_map(|field| {
            parse_connection_number(*field, self.text(*field))
                .err()
                .map(|problem| (*field, problem))
        });
    }

    /// Remembers the last usable interval, so switching keepalive off and on again does
    /// not silently fall back to the documented default.
    fn remember_interval(&mut self) {
        if let Ok(value) =
            parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval)
            && value > 0
        {
            self.last_interval = value;
        }
    }

    /// Whether keepalive probes are sent at all.
    #[must_use]
    pub fn keepalive_on(&self) -> bool {
        match parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval) {
            Ok(value) => value > 0,
            Err(_) => self.last_interval > 0,
        }
    }

    /// Switches keepalive on or off, writing zero for off as OpenSSH does.
    pub fn set_keepalive(&mut self, on: bool) {
        let text = if on {
            self.last_interval.max(1).to_string()
        } else {
            "0".to_owned()
        };
        self.set_text(ConnectionField::KeepaliveInterval, text);
    }

    /// The seconds without a reply before a connection counts as lost, when keepalive is
    /// on and both numbers are usable.
    #[must_use]
    pub fn detection_window_secs(&self) -> Option<u64> {
        let interval =
            parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval).ok()?;
        if interval == 0 {
            return None;
        }
        let misses =
            parse_connection_number(ConnectionField::KeepaliveMisses, &self.misses).ok()?;
        Some(detection_window_secs(interval, misses))
    }

    /// The defaults these fields describe.
    ///
    /// # Errors
    ///
    /// Returns the field and the reason when any of the three is unusable. Nothing is
    /// corrected on the way: the caller keeps editing the same text.
    pub fn resolve(&self) -> Result<ConnectionDefaults, (ConnectionField, ConnectionInputError)> {
        let interval = parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval)
            .map_err(|problem| (ConnectionField::KeepaliveInterval, problem))?;
        let misses = parse_connection_number(ConnectionField::KeepaliveMisses, &self.misses)
            .map_err(|problem| (ConnectionField::KeepaliveMisses, problem))?;
        let timeout = parse_connection_number(ConnectionField::ConnectTimeout, &self.timeout)
            .map_err(|problem| (ConnectionField::ConnectTimeout, problem))?;
        ConnectionDefaults::new(interval, misses, timeout).map_err(|error| {
            (
                ConnectionField::of_error(&error),
                ConnectionInputError::Refused(error),
            )
        })
    }
}

/// What one host says about its own keepalive (R20).
///
/// The three states are kept apart because they mean different things: inheriting
/// follows the global default wherever it goes, off keeps the default from applying at
/// all, and an override states its own numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionChoice {
    /// Nothing is stored; the global default applies.
    Inherit,
    /// This host sends no keepalive probe.
    Off,
    /// This host states its own interval, probe count and timeout.
    Override,
}

impl ConnectionChoice {
    /// The three choices, in the order the form offers them.
    pub const ALL: [Self; 3] = [Self::Inherit, Self::Off, Self::Override];

    /// Stable identifier, used by the selectors and the capture scenarios.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Off => "off",
            Self::Override => "override",
        }
    }

    /// The choice in one language.
    #[must_use]
    pub fn label(self, ux: &UxStrings) -> &'static str {
        match self {
            Self::Inherit => ux.host_connection_inherit,
            Self::Off => ux.host_connection_off,
            Self::Override => ux.host_connection_override,
        }
    }
}

/// One host's keepalive choice, as the host form edits it.
///
/// The override numbers start from what the host would be dialled with today - its own
/// value where it states one, the global default otherwise - so choosing "override" on a
/// host that inherits does not silently change what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConnectionDraft {
    choice: ConnectionChoice,
    interval: String,
    misses: String,
    timeout: String,
    last_interval: u32,
    error: Option<(ConnectionField, ConnectionInputError)>,
}

impl HostConnectionDraft {
    /// The form's copy of one host's record, resolved against the global defaults.
    #[must_use]
    pub fn of(record: Option<HostConnection>, defaults: ConnectionDefaults) -> Self {
        let stated = record.unwrap_or_default();
        let choice = match stated.keepalive_interval_secs() {
            // Off is the whole record that turns every keepalive field off. A host that
            // turns probing off but states its own timeout is an override, and reading it
            // as Off would drop that timeout when the form is saved (R20).
            Some(0)
                if stated.keepalive_max_misses().is_none()
                    && stated.connect_timeout_secs().is_none() =>
            {
                ConnectionChoice::Off
            }
            _ if !stated.is_empty() => ConnectionChoice::Override,
            _ => ConnectionChoice::Inherit,
        };
        // A host that says nothing, and a keepalive that is off everywhere, both leave the
        // override fields with no usable number to start from; the documented default is
        // the honest starting point, and the field is what changes it.
        let interval = stated.keepalive_interval_secs().unwrap_or_else(|| {
            let configured = defaults.keepalive_interval_secs();
            if configured == 0 {
                DEFAULT_KEEPALIVE_INTERVAL_SECS
            } else {
                configured
            }
        });
        Self {
            choice,
            interval: interval.to_string(),
            misses: stated
                .keepalive_max_misses()
                .unwrap_or(defaults.keepalive_max_misses())
                .to_string(),
            timeout: stated
                .connect_timeout_secs()
                .unwrap_or(defaults.connect_timeout_secs())
                .to_string(),
            last_interval: interval,
            error: None,
        }
    }

    /// The choice this form makes.
    #[must_use]
    pub fn choice(&self) -> ConnectionChoice {
        self.choice
    }

    /// Chooses inherit, off or override.
    pub fn set_choice(&mut self, choice: ConnectionChoice) -> bool {
        let changed = self.choice != choice;
        self.choice = choice;
        changed
    }

    /// The text of one override field.
    #[must_use]
    pub fn text(&self, field: ConnectionField) -> &str {
        match field {
            ConnectionField::KeepaliveInterval => &self.interval,
            ConnectionField::KeepaliveMisses => &self.misses,
            ConnectionField::ConnectTimeout => &self.timeout,
        }
    }

    /// What the last edit refused, and which field it was.
    #[must_use]
    pub fn error(&self) -> Option<(ConnectionField, &ConnectionInputError)> {
        self.error
            .as_ref()
            .map(|(field, problem)| (*field, problem))
    }

    /// Replaces one override field, keeping the reason if it is unusable.
    pub fn set_text(&mut self, field: ConnectionField, text: impl Into<String>) -> bool {
        let text = text.into();
        let slot = match field {
            ConnectionField::KeepaliveInterval => &mut self.interval,
            ConnectionField::KeepaliveMisses => &mut self.misses,
            ConnectionField::ConnectTimeout => &mut self.timeout,
        };
        if *slot == text {
            return false;
        }
        *slot = text;
        self.error = ConnectionField::ALL.iter().find_map(|field| {
            parse_connection_number(*field, self.text(*field))
                .err()
                .map(|problem| (*field, problem))
        });
        if let Ok(value) =
            parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval)
            && value > 0
        {
            self.last_interval = value;
        }
        true
    }

    /// The seconds without a reply before this connection counts as lost, while the form
    /// says keepalive is on.
    #[must_use]
    pub fn detection_window_secs(&self) -> Option<u64> {
        if self.choice != ConnectionChoice::Override {
            return None;
        }
        let interval =
            parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval).ok()?;
        if interval == 0 {
            return None;
        }
        let misses =
            parse_connection_number(ConnectionField::KeepaliveMisses, &self.misses).ok()?;
        Some(detection_window_secs(interval, misses))
    }

    /// The record this form describes: none for inherit, the switch that turns the probe
    /// off, or the three numbers.
    ///
    /// # Errors
    ///
    /// Returns the field and the reason when an override number is unusable.
    pub fn resolve(
        &self,
    ) -> Result<Option<HostConnection>, (ConnectionField, ConnectionInputError)> {
        match self.choice {
            ConnectionChoice::Inherit => Ok(None),
            ConnectionChoice::Off => Ok(Some(HostConnection::keepalive_off())),
            ConnectionChoice::Override => {
                let interval =
                    parse_connection_number(ConnectionField::KeepaliveInterval, &self.interval)
                        .map_err(|problem| (ConnectionField::KeepaliveInterval, problem))?;
                let misses =
                    parse_connection_number(ConnectionField::KeepaliveMisses, &self.misses)
                        .map_err(|problem| (ConnectionField::KeepaliveMisses, problem))?;
                let timeout =
                    parse_connection_number(ConnectionField::ConnectTimeout, &self.timeout)
                        .map_err(|problem| (ConnectionField::ConnectTimeout, problem))?;
                HostConnection::new(Some(interval), Some(misses), Some(timeout))
                    .map(Some)
                    .map_err(|error| {
                        (
                            ConnectionField::of_error(&error),
                            ConnectionInputError::Refused(error),
                        )
                    })
            }
        }
    }
}

/// The text of one host form.
///
/// The values stay text until the form is saved: a port of "0" has to be visible in
/// the field that produced it, so the editor keeps the text and asks the domain
/// whether it is usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDraft {
    editing: Option<HostId>,
    name: String,
    address: String,
    port: String,
    user: String,
    group: String,
    identity: String,
    /// How the host signs in.
    auth: AuthMethodKind,
    /// Whether the password or passphrase is kept in the system credential store.
    remember: bool,
    /// Whether the store already holds one for this host, so an empty field keeps it.
    has_saved_secret: bool,
    /// The secret typed in this form; empty means "keep what is saved".
    secret: String,
    startup: String,
    /// Saved hosts to dial before this one, in order.
    jumps: Vec<HostId>,
    /// ProxyCommand template. Blank clears it and nothing is executed.
    proxy: String,
    /// The protocol this host's transfers start with (R10).
    ///
    /// SFTP unless changed here. A single transfer may still choose the other one; that
    /// choice belongs to the task and does not change this host.
    protocol: TransferProtocol,
    /// How new sessions to this host forward X11 (R11).
    ///
    /// Off unless chosen here. A single connection may still choose another mode; that
    /// choice belongs to the attempt and does not change this host.
    x11: X11Forwarding,
    /// What this host says about its own keepalive and timeout (R20).
    ///
    /// The record the form opened with is kept beside the three-state choice, so the
    /// numbers an override starts from can be refreshed once the preferences are known.
    connection: HostConnectionDraft,
    connection_record: Option<HostConnection>,
}

impl HostDraft {
    /// An empty form for a new host.
    #[must_use]
    pub fn create() -> Self {
        Self {
            editing: None,
            name: String::new(),
            address: String::new(),
            port: Port::DEFAULT.to_string(),
            user: String::new(),
            group: String::new(),
            identity: String::new(),
            auth: AuthMethodKind::Password,
            remember: true,
            has_saved_secret: false,
            secret: String::new(),
            startup: String::new(),
            jumps: Vec::new(),
            proxy: String::new(),
            protocol: TransferProtocol::Sftp,
            x11: X11Forwarding::Off,
            connection: HostConnectionDraft::of(None, ConnectionDefaults::default()),
            connection_record: None,
        }
    }

    /// A form filled with a saved host.
    #[must_use]
    pub fn edit(host: &HostConfig) -> Self {
        Self {
            editing: Some(host.id()),
            name: host.name().to_owned(),
            address: host.address().to_owned(),
            port: host.port().to_string(),
            user: host.user().to_owned(),
            // A host in a nested group stores only its path; the label field is what an
            // earlier build wrote. Showing the canonical path keeps the form honest and
            // saving it cannot flatten the host into a look-alike group (R4, ADR-8).
            group: host
                .group_path()
                .map(|path| path.to_string())
                .or_else(|| host.group().map(str::to_owned))
                .unwrap_or_default(),
            identity: host.identity_file().unwrap_or_default().to_owned(),
            // A host saved before the choice existed signs in with its key when it has
            // one, and with a password otherwise, which is what its form offered.
            auth: host.auth().unwrap_or(if host.identity_file().is_some() {
                AuthMethodKind::KeyFile
            } else {
                AuthMethodKind::Password
            }),
            remember: host.remembers_secret() || host.auth().is_none(),
            has_saved_secret: host.remembers_secret(),
            secret: String::new(),
            startup: host.startup_command().unwrap_or_default().to_owned(),
            jumps: host.jumps().to_vec(),
            proxy: host.proxy_command().unwrap_or_default().to_owned(),
            protocol: host.transfer_protocol(),
            x11: host.x11(),
            connection: HostConnectionDraft::of(host.connection(), ConnectionDefaults::default()),
            connection_record: host.connection(),
        }
    }

    /// The protocol this host's transfers start with.
    #[must_use]
    pub fn protocol(&self) -> TransferProtocol {
        self.protocol
    }

    /// Chooses the protocol this host's transfers start with, returning whether it changed.
    pub fn set_protocol(&mut self, protocol: TransferProtocol) -> bool {
        let changed = self.protocol != protocol;
        self.protocol = protocol;
        changed
    }

    /// How new sessions to this host forward X11.
    #[must_use]
    pub fn x11(&self) -> X11Forwarding {
        self.x11
    }

    /// Chooses the X11 mode of new sessions, returning whether it changed.
    pub fn set_x11(&mut self, mode: X11Forwarding) -> bool {
        let changed = self.x11 != mode;
        self.x11 = mode;
        changed
    }

    /// This host's keepalive choice, as the form edits it (R20).
    #[must_use]
    pub fn connection(&self) -> &HostConnectionDraft {
        &self.connection
    }

    /// The keepalive choice, to be changed by the form.
    pub fn connection_mut(&mut self) -> &mut HostConnectionDraft {
        &mut self.connection
    }

    /// Re-reads the numbers a host that inherits would be dialled with.
    ///
    /// The form is built before the preferences are known in some paths, so the override
    /// fields are refreshed once from the defaults in force instead of being frozen at
    /// construction. Nothing the user typed is involved: this runs while the form opens.
    pub fn refresh_connection_defaults(&mut self, defaults: ConnectionDefaults) {
        self.connection = HostConnectionDraft::of(self.connection_record, defaults);
    }

    /// How the host signs in.
    #[must_use]
    pub fn auth(&self) -> AuthMethodKind {
        self.auth
    }

    /// Chooses how the host signs in, returning whether it changed.
    pub fn set_auth(&mut self, auth: AuthMethodKind) -> bool {
        let changed = self.auth != auth;
        self.auth = auth;
        changed
    }

    /// Whether the secret of this method is to be remembered.
    #[must_use]
    pub fn remember(&self) -> bool {
        self.remember
    }

    pub fn set_remember(&mut self, remember: bool) {
        self.remember = remember;
    }

    /// Whether the system store already holds this host's secret.
    #[must_use]
    pub fn has_saved_secret(&self) -> bool {
        self.has_saved_secret
    }

    /// Whether the chosen method reads a password or a passphrase.
    #[must_use]
    pub fn needs_secret(&self) -> bool {
        self.auth.needs_password() || self.auth.needs_passphrase()
    }

    /// Whether the form will keep a secret in the system store when saved.
    ///
    /// Only when there is one to keep: a secret typed now, or one already saved. A
    /// host saved with the box ticked and nothing typed signs in by asking, which is
    /// what "save it now, type the password when connecting" needs.
    #[must_use]
    pub fn keeps_secret(&self) -> bool {
        self.needs_secret() && self.remember && (!self.secret.is_empty() || self.has_saved_secret)
    }

    /// The secret typed in this form, when one was typed.
    #[must_use]
    pub fn typed_secret(&self) -> Option<&str> {
        (!self.secret.is_empty()).then_some(self.secret.as_str())
    }

    /// Forgets the typed secret, once it has been handed to the store.
    pub fn clear_secret(&mut self) {
        self.secret.clear();
    }

    /// The host this form changes, when it is not a new one.
    #[must_use]
    pub fn editing(&self) -> Option<HostId> {
        self.editing
    }

    /// The current text of one field.
    #[must_use]
    pub fn text(&self, field: HostField) -> &str {
        match field {
            HostField::Name => &self.name,
            HostField::Address => &self.address,
            HostField::Port => &self.port,
            HostField::User => &self.user,
            HostField::Group => &self.group,
            HostField::IdentityFile => &self.identity,
            HostField::Secret => &self.secret,
            HostField::StartupCommand => &self.startup,
            HostField::ProxyCommand => &self.proxy,
        }
    }

    /// Replaces the text of one field, returning whether it changed.
    pub fn set_text(&mut self, field: HostField, text: impl Into<String>) -> bool {
        let slot = match field {
            HostField::Name => &mut self.name,
            HostField::Address => &mut self.address,
            HostField::Port => &mut self.port,
            HostField::User => &mut self.user,
            HostField::Group => &mut self.group,
            HostField::IdentityFile => &mut self.identity,
            HostField::Secret => &mut self.secret,
            HostField::StartupCommand => &mut self.startup,
            HostField::ProxyCommand => &mut self.proxy,
        };
        let text = text.into();
        if *slot == text {
            return false;
        }
        *slot = text;
        true
    }

    /// The validated change this form describes.
    ///
    /// # Errors
    ///
    /// Returns the reason the domain rejected the values: an empty name, address or
    /// user, an address with whitespace, or a port outside `1..=65535`. The text is
    /// not changed, so the user can correct the field that was refused.
    pub fn build(&self, existing: Option<&HostConfig>) -> Result<HostEdit, String> {
        let port = parse_port(&self.port)?;
        match existing {
            Some(saved) => {
                let mut host = saved.clone();
                host.set_name(self.name.clone())
                    .map_err(|error| error.to_string())?;
                host.set_address(self.address.clone())
                    .map_err(|error| error.to_string())?;
                host.set_port(u32::from(port.get()))
                    .map_err(|error| error.to_string())?;
                host.set_user(self.user.clone())
                    .map_err(|error| error.to_string())?;
                self.apply_group(&mut host)?;
                // A blank field clears the path instead of storing an empty string, and
                // the value is only ever the path to the key.
                host.set_identity_file(self.identity_for_method());
                self.apply_sign_in(&mut host);
                self.apply_route(&mut host);
                self.apply_connection(&mut host)?;
                Ok(HostEdit::replace(host))
            }
            None => {
                let mut host = HostConfig::new(
                    self.name.clone(),
                    self.address.clone(),
                    port.get(),
                    self.user.clone(),
                )
                .map_err(|error| error.to_string())?;
                self.apply_group(&mut host)?;
                host.set_identity_file(self.identity_for_method());
                self.apply_sign_in(&mut host);
                self.apply_route(&mut host);
                self.apply_connection(&mut host)?;
                Ok(HostEdit::create(host))
            }
        }
    }

    /// Applies the group field as the canonical path it names.
    ///
    /// A blank field means no group; anything else is parsed with the same rules the
    /// group tree uses, so `Prod/Web` is two levels rather than one label containing a
    /// separator (ADR-8). The reason is the form's message when the path is refused.
    fn apply_group(&self, host: &mut HostConfig) -> Result<(), String> {
        let text = self.group.trim();
        if text.is_empty() {
            host.set_group_path(None);
            return Ok(());
        }
        let path = GroupPath::parse(text).map_err(|error| error.to_string())?;
        host.set_group_path(Some(path));
        Ok(())
    }

    /// The jump hosts, in dial order.
    #[must_use]
    pub fn jumps(&self) -> &[HostId] {
        &self.jumps
    }

    /// Adds a saved host to the end of the jump chain.
    ///
    /// The host being edited is not added, and a hop already in the list is not repeated.
    pub fn add_jump(&mut self, id: HostId) -> bool {
        if self.editing == Some(id) || self.jumps.contains(&id) {
            return false;
        }
        self.jumps.push(id);
        true
    }

    /// Removes the hop at `index`.
    pub fn remove_jump(&mut self, index: usize) {
        if index < self.jumps.len() {
            self.jumps.remove(index);
        }
    }

    /// Moves the hop at `index` one place up or down.
    pub fn move_jump(&mut self, index: usize, up: bool) {
        let Some(next) = (if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        }) else {
            return;
        };
        if next < self.jumps.len() {
            self.jumps.swap(index, next);
        }
    }

    /// Whether the form currently asks for both a jump chain and a ProxyCommand.
    #[must_use]
    pub fn proxy_conflicts(&self) -> bool {
        !self.jumps.is_empty() && !self.proxy.trim().is_empty()
    }

    /// Writes the jump chain, the ProxyCommand, the copy protocol and the X11 mode.
    ///
    /// A blank command clears it, and the SFTP default is stored as no choice at all, so
    /// a host that was never changed around these fields keeps the record it had.
    fn apply_route(&self, host: &mut HostConfig) {
        host.set_jumps(self.jumps.iter().copied());
        host.set_proxy_command(Some(self.proxy.clone()));
        host.set_transfer_protocol(self.protocol);
        host.set_x11(self.x11);
    }

    /// Writes this host's own keepalive numbers, or clears them when it inherits.
    ///
    /// A number the SSH layer would refuse is not stored and not corrected: the form
    /// stays open with the reason, exactly like a port outside 1-65535 (R20).
    fn apply_connection(&self, host: &mut HostConfig) -> Result<(), String> {
        let record = self
            .connection
            .resolve()
            .map_err(|(field, problem)| connection_refusal(field, &problem))?;
        host.set_connection(record);
        Ok(())
    }

    /// The key path to store: kept only for a method that reads a key file.
    fn identity_for_method(&self) -> Option<String> {
        self.auth.needs_key_path().then(|| self.identity.clone())
    }

    /// Writes the sign-in choice and the startup command into a record.
    fn apply_sign_in(&self, host: &mut HostConfig) {
        host.set_auth(Some(self.auth));
        host.set_remember_secret(self.keeps_secret());
        host.set_startup_command(Some(self.startup.clone()));
    }
}

/// The reason a host form's keepalive numbers were refused, in one sentence.
///
/// The sentence names the field, because the form has three numbers and the user has to
/// know which one to correct. The wording is the domain's own for a range refusal so the
/// limit it names is the limit that really applies.
fn connection_refusal(field: ConnectionField, problem: &ConnectionInputError) -> String {
    let name = field.id().replace('-', " ");
    match problem {
        ConnectionInputError::Missing => format!("{name} is empty; type a whole number"),
        ConnectionInputError::NotANumber { text } => {
            format!("{name}: {text} is not a whole number")
        }
        ConnectionInputError::Negative { text } => format!("{name}: {text} is below zero"),
        ConnectionInputError::Refused(error) => format!("{name}: {error}"),
    }
}

/// Parses the port field.
///
/// An empty field means the SSH default; anything else goes through the domain's
/// range check, which is the only place that decides whether a port is usable.
fn parse_port(text: &str) -> Result<Port, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Port::DEFAULT);
    }
    let raw: u32 = text
        .parse()
        .map_err(|_| format!("{text} is not a port number; use 1-65535"))?;
    Port::new(raw).map_err(|error| error.to_string())
}

/// What the last action produced, in the words of the layer that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostsMessage {
    text: String,
    failed: bool,
}

impl HostsMessage {
    /// The text to show.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the action failed, which decides the tone.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed
    }
}

/// Everything the hosts destination holds apart from the records themselves.
#[derive(Debug, Default)]
pub struct HostsPanel {
    view: HostView,
    search: String,
    draft: Option<HostDraft>,
    pending_delete: Option<HostId>,
    message: Option<HostsMessage>,
    /// The group and status the list is narrowed to, beside the search phrase.
    filter: HostFilter,
    /// What the selection means while it grows: the current result or every host.
    scope: SelectionScope,
    /// The hosts the user picked, by identifier rather than by row (R18).
    selected: HostSelection,
    /// The groups the tree draws closed. A group starts open.
    collapsed: BTreeSet<GroupPath>,
    /// Whether the hosts without a group are drawn closed.
    ungrouped_collapsed: bool,
    /// The group being created, renamed, moved into or deleted, while its dialog is up.
    group_editor: Option<GroupEditor>,
}

impl HostsPanel {
    /// A panel showing every saved host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current list view.
    #[must_use]
    pub fn view(&self) -> HostView {
        self.view
    }

    /// Switches the list view and clears a pending question, which belonged to what
    /// the user was looking at.
    pub fn select_view(&mut self, view: HostView) {
        if self.view != view {
            self.view = view;
            self.pending_delete = None;
        }
    }

    /// The current search phrase.
    #[must_use]
    pub fn search(&self) -> &str {
        &self.search
    }

    /// Sets the search phrase, which every view applies.
    pub fn set_search(&mut self, phrase: impl Into<String>) {
        self.search = phrase.into();
    }

    /// The open form, if any.
    #[must_use]
    pub fn draft(&self) -> Option<&HostDraft> {
        self.draft.as_ref()
    }

    /// The open form, for the fields to write into.
    pub fn draft_mut(&mut self) -> Option<&mut HostDraft> {
        self.draft.as_mut()
    }

    /// Whether a form is open.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.draft.is_some()
    }

    /// Opens an empty form for a new host.
    /// Re-reads the numbers a form that inherits would be dialled with (R20).
    ///
    /// Called as the dialog opens, before anything is typed.
    pub fn apply_connection_defaults(&mut self, defaults: ConnectionDefaults) {
        if let Some(draft) = self.draft.as_mut() {
            draft.refresh_connection_defaults(defaults);
        }
    }

    pub fn open_create(&mut self) {
        self.draft = Some(HostDraft::create());
        self.pending_delete = None;
        self.clear_message();
    }

    /// Opens the form filled with a saved host.
    pub fn open_edit(&mut self, host: &HostConfig) {
        self.draft = Some(HostDraft::edit(host));
        self.pending_delete = None;
        self.clear_message();
    }

    /// Closes the form, keeping what was typed out of the records.
    pub fn close_editor(&mut self) {
        self.draft = None;
    }

    /// The host the user is about to delete.
    #[must_use]
    pub fn pending_delete(&self) -> Option<HostId> {
        self.pending_delete
    }

    /// Asks the deletion question for one host.
    pub fn ask_delete(&mut self, id: HostId) {
        self.pending_delete = Some(id);
        self.draft = None;
    }

    /// Drops the deletion question without touching the records.
    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    /// Takes the confirmed deletion target.
    pub fn take_delete(&mut self) -> Option<HostId> {
        self.pending_delete.take()
    }

    /// The last message, if any.
    #[must_use]
    pub fn message(&self) -> Option<&HostsMessage> {
        self.message.as_ref()
    }

    /// Records what the last action produced.
    pub fn set_message(&mut self, text: impl Into<String>, failed: bool) {
        self.message = Some(HostsMessage {
            text: text.into(),
            failed,
        });
    }

    /// Removes the last message.
    pub fn clear_message(&mut self) {
        self.message = None;
    }

    /// The group and status filters in force beside the search phrase.
    #[must_use]
    pub fn filter(&self) -> &HostFilter {
        &self.filter
    }

    /// Narrows the list to one group and everything inside it.
    ///
    /// The filter is a place in the tree, not a row: a group that is collapsed or
    /// hidden by the narrow layout still narrows the list, and dropping the filter is
    /// the only way back to every group.
    pub fn set_group_filter(&mut self, group: Option<GroupPath>) {
        if self.filter.group != group {
            self.filter.group = group;
            self.pending_delete = None;
        }
    }

    /// Narrows the list by the connection state this window already knows.
    pub fn set_status_filter(&mut self, status: HostStatusFilter) {
        if self.filter.status != status {
            self.filter.status = status;
            self.pending_delete = None;
        }
    }

    /// Drops every narrowing filter, keeping the search phrase as it is.
    pub fn clear_filters(&mut self) {
        self.filter = HostFilter::default();
    }

    /// What the selection covers while it grows.
    #[must_use]
    pub fn scope(&self) -> SelectionScope {
        self.scope
    }

    /// Chooses what a selection means: the current result, or every saved host.
    pub fn set_scope(&mut self, scope: SelectionScope) {
        self.scope = scope;
    }

    /// The hosts the user picked.
    #[must_use]
    pub fn selected(&self) -> &HostSelection {
        &self.selected
    }

    /// Adds or removes one host from the selection.
    ///
    /// Returns whether the host is selected afterwards.
    pub fn toggle_selected(&mut self, id: HostId) -> bool {
        self.selected.toggle(id)
    }

    /// Replaces the selection with the given hosts.
    pub fn select(&mut self, ids: impl IntoIterator<Item = HostId>) {
        self.selected.replace(ids);
    }

    /// Empties the selection.
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// How many hosts are selected.
    #[must_use]
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Drops selected hosts the book no longer holds.
    ///
    /// A deleted host leaves the selection with it: a count of hosts that are no
    /// longer saved would promise a move that can never happen.
    pub fn retain_selected(&mut self, book: &HostBook) {
        let saved: Vec<HostId> = book.hosts().iter().map(HostConfig::id).collect();
        self.selected.retain(&saved);
    }

    /// Whether the tree draws one group open.
    #[must_use]
    pub fn is_expanded(&self, path: &GroupPath) -> bool {
        !self.collapsed.contains(path)
    }

    /// Opens or closes one group in the tree, changing only what is drawn.
    pub fn toggle_group(&mut self, path: &GroupPath) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.clone());
        }
    }

    /// Sets the tree's state for one group without toggling it.
    pub fn set_expanded(&mut self, path: &GroupPath, expanded: bool) {
        if expanded {
            self.collapsed.remove(path);
        } else {
            self.collapsed.insert(path.clone());
        }
    }

    /// The group change being asked about, while its dialog is open.
    #[must_use]
    pub fn group_editor(&self) -> Option<&GroupEditor> {
        self.group_editor.as_ref()
    }

    /// The group change being asked about, for the dialog's own fields.
    pub fn group_editor_mut(&mut self) -> Option<&mut GroupEditor> {
        self.group_editor.as_mut()
    }

    /// Opens the dialog for one group change.
    pub fn open_group_dialog(&mut self, action: GroupAction) {
        let name = match &action {
            GroupAction::Rename(path) => path.to_canonical(),
            _ => String::new(),
        };
        self.group_editor = Some(GroupEditor::new(action, name));
        self.draft = None;
        self.pending_delete = None;
        self.clear_message();
    }

    /// Closes the dialog, leaving the tree as it was.
    pub fn close_group_editor(&mut self) {
        self.group_editor = None;
    }

    /// The flattened rows the list draws, and the hosts the filters keep.
    ///
    /// The rows are the same for the wide window and the narrow one: the layout only
    /// decides where the group chooser is drawn. A collapsed group hides its rows but
    /// not its hosts, so the count in the header and the result the filters keep always
    /// agree.
    ///
    /// [open] reports whether this window already holds a live session for a host. It
    /// is a read of existing state, never a request: narrowing by "connected" cannot
    /// start a connection (R18).
    #[must_use]
    pub fn list(
        &self,
        book: &HostBook,
        groups: &GroupBook,
        open: &dyn Fn(HostId) -> bool,
    ) -> HostList {
        let all: Vec<HostId> = book.hosts().iter().map(HostConfig::id).collect();
        let mut matched: Vec<HostId> = book
            .hosts()
            .iter()
            .filter(|host| self.matches(book, host, open))
            .map(HostConfig::id)
            .collect();
        let kept: BTreeSet<HostId> = matched.iter().copied().collect();
        let narrow_search = !self.search.trim().is_empty();
        let mut rows = Vec::new();
        match self.view {
            HostView::All => {
                for id in &matched {
                    rows.push(HostListRow::Host(HostRow { id: *id }));
                }
            }
            HostView::Recent => {
                // The recent view is itself a filter: a host with no record is not in
                // the list, so it must not be counted in "current results" either, or
                // Select would pick hosts the user cannot see (R18).
                let mut shown = Vec::new();
                for recent in book.recent_hosts() {
                    let id = recent.host.id();
                    if kept.contains(&id) {
                        shown.push(id);
                        rows.push(HostListRow::Host(HostRow { id }));
                    }
                }
                matched = shown;
            }
            HostView::Groups => {
                self.push_tree(&mut rows, book, groups, &kept, narrow_search);
            }
        }
        HostList { rows, matched, all }
    }

    /// The group tree on its own, whatever view the list is in (R4, R18).
    ///
    /// The column that navigates the tree is not the list: it exists beside a flat view
    /// too, and it has to name the same groups with the same counts as the list would if
    /// it were grouped. That is why both are built from one function.
    #[must_use]
    pub fn group_tree(
        &self,
        book: &HostBook,
        groups: &GroupBook,
        open: &dyn Fn(HostId) -> bool,
    ) -> Vec<HostGroupRow> {
        // The tree is where the group filter is chosen, so it cannot be narrowed by it:
        // a tree that shows only the branch already chosen would leave no way to choose
        // another. The search and the status still count, because they say how much a
        // branch holds of what the rest of the page is looking at.
        let kept: BTreeSet<HostId> = book
            .hosts()
            .iter()
            .filter(|host| self.matches_without_group(book, host, open))
            .map(HostConfig::id)
            .collect();
        let narrow_search = !self.search.trim().is_empty();
        let mut rows = Vec::new();
        self.push_tree(&mut rows, book, groups, &kept, narrow_search);
        rows.into_iter()
            .filter_map(|row| match row {
                HostListRow::Group(group) => Some(group),
                HostListRow::Host(_) => None,
            })
            .collect()
    }

    /// Writes the headers of the whole tree, in order.
    fn push_tree(
        &self,
        rows: &mut Vec<HostListRow>,
        book: &HostBook,
        groups: &GroupBook,
        kept: &BTreeSet<HostId>,
        narrow_search: bool,
    ) {
        let paths = drawn_paths(book, groups);
        for path in child_paths(&paths, None) {
            self.push_group(rows, &paths, book, kept, path, 0, narrow_search);
        }
        self.push_ungrouped(rows, book, kept);
    }

    /// Writes one group of the tree, its children and its own hosts.
    ///
    /// Seven arguments: the rows being written, the tree, the hosts, the ones the
    /// filters keep, the group, its depth, and whether a phrase is typed. Splitting them
    /// into a struct would only move the same fields behind a name.
    #[allow(clippy::too_many_arguments)]
    ///
    /// A group the search does not reach is left out while a phrase is typed, so the
    /// tree answers the search instead of listing every group with a zero beside it. A
    /// group that is only collapsed keeps its row and its count, because closing a
    /// group is a decision about what to see, not a filter.
    fn push_group(
        &self,
        rows: &mut Vec<HostListRow>,
        paths: &[GroupPath],
        book: &HostBook,
        kept: &BTreeSet<HostId>,
        path: GroupPath,
        depth: usize,
        narrow_search: bool,
    ) {
        let hosts = hosts_under(book, kept, &path);
        if narrow_search && hosts.is_empty() {
            return;
        }
        let expanded = self.is_expanded(&path);
        rows.push(HostListRow::Group(HostGroupRow {
            path: Some(path.clone()),
            depth,
            hosts: hosts.len(),
            expanded,
        }));
        if !expanded {
            return;
        }
        for child in child_paths(paths, Some(&path)) {
            self.push_group(rows, paths, book, kept, child, depth + 1, narrow_search);
        }
        for id in hosts {
            rows.push(HostListRow::Host(HostRow { id }));
        }
    }

    /// Writes the hosts that belong to no group, under a header of their own.
    ///
    /// They are a real place in the list rather than a leftover: a host without a group
    /// has to be as reachable as one inside a group, and a bulk move has to be able to
    /// name them.
    fn push_ungrouped(
        &self,
        rows: &mut Vec<HostListRow>,
        book: &HostBook,
        kept: &BTreeSet<HostId>,
    ) {
        let hosts: Vec<HostId> = book
            .hosts()
            .iter()
            .filter(|host| kept.contains(&host.id()) && host.group_path().is_none())
            .map(HostConfig::id)
            .collect();
        if hosts.is_empty() {
            return;
        }
        let expanded = self.ungrouped_expanded();
        rows.push(HostListRow::Group(HostGroupRow {
            path: None,
            depth: 0,
            hosts: hosts.len(),
            expanded,
        }));
        if expanded {
            for id in hosts {
                rows.push(HostListRow::Host(HostRow { id }));
            }
        }
    }

    /// Whether the ungrouped bucket is drawn open.
    ///
    /// It has no path to store in the collapsed set, so the panel keeps its state as a
    /// flag next to the tree. It starts open, like every group.
    #[must_use]
    pub fn ungrouped_expanded(&self) -> bool {
        !self.ungrouped_collapsed
    }

    /// Opens or closes the ungrouped bucket.
    pub fn toggle_ungrouped(&mut self) {
        self.ungrouped_collapsed = !self.ungrouped_collapsed;
    }

    /// Sets whether the ungrouped bucket is drawn open.
    pub fn set_ungrouped_expanded(&mut self, expanded: bool) {
        self.ungrouped_collapsed = !expanded;
    }

    /// Whether one host survives the search phrase, the group and the status.
    fn matches(&self, book: &HostBook, host: &HostConfig, open: &dyn Fn(HostId) -> bool) -> bool {
        if !host_matches(host, &self.search) {
            return false;
        }
        if let Some(wanted) = &self.filter.group {
            match host.group_path() {
                Some(path) if path.starts_with(wanted) => {}
                _ => return false,
            }
        }
        self.matches_status(book, host, open)
    }

    /// Whether one host survives everything but the group, for the tree that chooses it.
    fn matches_without_group(
        &self,
        book: &HostBook,
        host: &HostConfig,
        open: &dyn Fn(HostId) -> bool,
    ) -> bool {
        host_matches(host, &self.search) && self.matches_status(book, host, open)
    }

    /// Whether one host survives the status filter.
    fn matches_status(
        &self,
        book: &HostBook,
        host: &HostConfig,
        open: &dyn Fn(HostId) -> bool,
    ) -> bool {
        match self.filter.status {
            HostStatusFilter::Any => true,
            HostStatusFilter::Connected => open(host.id()),
            HostStatusFilter::Recent => book.recent_of(host.id()).is_some(),
            HostStatusFilter::NeverConnected => book.recent_of(host.id()).is_none(),
        }
    }
}

/// The import panel: which configuration to read, and what reading it showed.
///
/// The panel holds the result of one read rather than a live view of the file.
/// Changing the path drops the previous preview, so the hosts that can be applied are
/// always the ones that were shown for that path; the file itself is never opened for
/// writing, which is why nothing here has a save path of its own.
#[derive(Debug)]
pub struct ImportPanel {
    open: bool,
    path: String,
    preview: Option<ImportPreview>,
    error: Option<String>,
    /// The phrase the preview is narrowed by (R18).
    ///
    /// A configuration file can describe thousands of hosts, and the dialog has to be
    /// readable at that size: the same "search a list" idea the servers page uses.
    search: String,
}

impl Default for ImportPanel {
    fn default() -> Self {
        Self {
            open: false,
            path: DEFAULT_CONFIG_TEXT.to_owned(),
            preview: None,
            error: None,
            search: String::new(),
        }
    }
}

impl ImportPanel {
    /// A closed panel pointing at the file the platform's ssh reads.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the panel is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the panel on a path, dropping whatever the previous path showed.
    pub fn open(&mut self, path: impl Into<String>) {
        self.open = true;
        self.path = path.into();
        self.preview = None;
        self.error = None;
    }

    /// Closes the panel. The path is kept for the next time it is opened.
    pub fn close(&mut self) {
        self.open = false;
        self.preview = None;
        self.error = None;
    }

    /// The path the user typed.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Replaces the path, dropping a result that belongs to the previous one.
    pub fn set_path(&mut self, path: impl Into<String>) {
        let path = path.into();
        if path != self.path {
            self.path = path;
            self.preview = None;
            self.error = None;
        }
    }

    /// What the last read showed.
    #[must_use]
    pub fn preview(&self) -> Option<&ImportPreview> {
        self.preview.as_ref()
    }

    /// Adopts a preview as the result to show.
    pub fn set_preview(&mut self, preview: ImportPreview) {
        self.preview = Some(preview);
        self.error = None;
    }

    /// Why the file could not be read or parsed.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The phrase the preview is narrowed by.
    #[must_use]
    pub fn search(&self) -> &str {
        &self.search
    }

    /// Narrows the preview to the hosts the phrase names.
    pub fn set_search(&mut self, phrase: impl Into<String>) {
        self.search = phrase.into();
    }

    /// The hosts of the preview the phrase keeps, with the position they were read at.
    ///
    /// The position is what the row's selector is built from, so a narrowed list keeps
    /// the identity of the line the file holds.
    #[must_use]
    pub fn matching(&self) -> Vec<(usize, &ImportedHost)> {
        let Some(preview) = &self.preview else {
            return Vec::new();
        };
        preview
            .hosts()
            .iter()
            .enumerate()
            .filter(|(_, host)| import_matches(host, &self.search))
            .collect()
    }

    /// Reports a read or parse failure, which no preview can survive.
    pub fn set_error(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
        self.preview = None;
    }

    /// Reports a failure that happened after the file was read.
    ///
    /// The preview stays on screen because it is still a true reading of the file, and
    /// the reason says why the hosts could not be written instead.
    pub fn set_apply_error(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
    }

    /// Whether the reason on screen belongs to a read that did produce a preview.
    #[must_use]
    pub fn keeps_preview(&self) -> bool {
        self.preview.is_some()
    }
}

/// How many rows of an import preview are built at once.
///
/// A row carries every reason the importer wrote for that host, so its height differs
/// from its neighbours' and the list cannot be a uniform one. The bound is the second
/// best answer: a file of thousands builds a bounded number of rows, the search narrows
/// what is left, and the count above the list says both numbers (R18).
pub const IMPORT_ROW_LIMIT: usize = 200;

/// Whether one host of a preview survives the phrase typed into the import dialog.
///
/// The alias and the summary are what the row shows; matching text a person cannot see
/// on the row would find hosts for a reason nobody could confirm.
#[must_use]
pub fn import_matches(host: &ImportedHost, phrase: &str) -> bool {
    let phrase = phrase.trim().to_lowercase();
    if phrase.is_empty() {
        return true;
    }
    host.alias().to_lowercase().contains(&phrase) || host.summary().to_lowercase().contains(&phrase)
}

/// Hosts the preview did not save, each with the reason the preview or the plan gave.
///
/// Blocked hosts and hosts missing a field never enter the plan. A name that is
/// already saved, or a jump that could not be linked, is listed on the plan instead.
#[must_use]
pub fn unimported_hosts(preview: &ImportPreview, plan: &ImportPlan) -> Vec<(String, String)> {
    let mut withheld = Vec::new();
    for host in preview.hosts() {
        if host.is_importable() {
            continue;
        }
        let reason = host
            .notes()
            .iter()
            .map(|note| note.reason())
            .collect::<Vec<_>>()
            .join("; ");
        let reason = if reason.is_empty() {
            match host.outcome() {
                ImportOutcome::Blocked => "Blocked".to_owned(),
                _ => "missing fields".to_owned(),
            }
        } else {
            reason
        };
        withheld.push((host.alias().to_owned(), reason));
    }
    for skipped in plan.skipped() {
        if withheld
            .iter()
            .any(|(alias, _)| alias.eq_ignore_ascii_case(skipped.alias()))
        {
            continue;
        }
        withheld.push((skipped.alias().to_owned(), skipped.reason().to_owned()));
    }
    withheld
}

/// The message an applied import leaves behind, and whether it is a failure.
///
/// The counts say what was written, what needs a look and what was skipped. Every
/// host that was not imported is named with its reason. That list makes the result
/// a failure: a count line on its own would read as a complete import.
#[must_use]
pub fn import_message(
    strings: &Strings,
    imported: usize,
    review: usize,
    skipped: usize,
    withheld: &[(String, String)],
) -> (String, bool) {
    let mut message = format!(
        "{} {imported} {} · {} {review} · {} {skipped}",
        strings.import_applied,
        strings.count_hosts,
        strings.import_review_count,
        strings.import_skipped
    );
    if withheld.is_empty() {
        return (message, false);
    }
    message.push_str(" · ");
    message.push_str(strings.import_not_offered);
    for (alias, reason) in withheld {
        message.push(' ');
        message.push_str(alias);
        message.push_str(": ");
        message.push_str(reason);
        message.push(';');
    }
    (message, true)
}

/// Formats a Unix timestamp as UTC, without pulling a date library into the shell.
///
/// The recent view has to say *when* a host was reached, and a stored integer is not
/// an answer a user can read. The conversion is the standard civil-from-days
/// algorithm, and the tests pin it to known instants.
#[must_use]
pub fn format_unix(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time / 3600,
        (time % 3600) / 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// What a batch selection covers while it grows (R18).
///
/// The two answers are kept apart on purpose: "every host the search keeps" and "every
/// saved host" produce different moves, and a count without its scope would let a
/// filtered list look like the whole book.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionScope {
    /// The hosts the current search and filters keep, whether or not they are in view.
    #[default]
    CurrentResults,
    /// Every saved host, whatever the filters say.
    AllHosts,
}

impl SelectionScope {
    /// Both scopes, in the order the control shows them.
    pub const ALL: [Self; 2] = [Self::CurrentResults, Self::AllHosts];

    /// Label of the scope, in the language of the window.
    #[must_use]
    pub fn label(self, ux: &UxStrings) -> &'static str {
        match self {
            Self::CurrentResults => ux.hosts_scope_current,
            Self::AllHosts => ux.hosts_scope_all,
        }
    }

    /// Debug selector of the scope control, used by the tests.
    #[must_use]
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::CurrentResults => "hosts.scope.current",
            Self::AllHosts => "hosts.scope.all",
        }
    }
}

/// The connection state a host list is narrowed by (R4, R18).
///
/// Every answer is read from data this window already holds: the sessions it has open
/// and the recent-connection log the host file carries. None of them asks a server
/// anything, so narrowing a list of five thousand hosts cannot start five thousand
/// connections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostStatusFilter {
    /// Every host, connected or not.
    #[default]
    Any,
    /// A host this window has an open session to.
    Connected,
    /// A host that was reached before, whether or not it is open now.
    Recent,
    /// A host the book has never recorded a connection for.
    NeverConnected,
}

impl HostStatusFilter {
    /// Every status, in the order the control shows them.
    pub const ALL: [Self; 4] = [
        Self::Any,
        Self::Connected,
        Self::Recent,
        Self::NeverConnected,
    ];

    /// Label of the status, in the language of the window.
    #[must_use]
    pub fn label(self, ux: &UxStrings) -> &'static str {
        match self {
            Self::Any => ux.hosts_status_any,
            Self::Connected => ux.hosts_status_connected,
            Self::Recent => ux.hosts_status_recent,
            Self::NeverConnected => ux.hosts_status_never,
        }
    }

    /// Debug selector of the status entry, used by the tests.
    #[must_use]
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::Any => "hosts.status.any",
            Self::Connected => "hosts.status.connected",
            Self::Recent => "hosts.status.recent",
            Self::NeverConnected => "hosts.status.never",
        }
    }
}

/// The narrowing the list applies beside the search phrase.
///
/// A group filter is a place in the tree and includes everything inside it, so picking
/// a parent is a way to see a whole branch without opening it row by row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostFilter {
    group: Option<GroupPath>,
    status: HostStatusFilter,
}

impl HostFilter {
    /// The group the list is narrowed to, when it is.
    #[must_use]
    pub fn group(&self) -> Option<&GroupPath> {
        self.group.as_ref()
    }

    /// The connection state the list is narrowed to.
    #[must_use]
    pub fn status(&self) -> HostStatusFilter {
        self.status
    }

    /// Whether either filter narrows anything.
    #[must_use]
    pub fn is_narrowing(&self) -> bool {
        self.group.is_some() || self.status != HostStatusFilter::Any
    }
}

/// One group header of the flattened list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostGroupRow {
    /// The group, or nothing for the hosts that belong to no group.
    pub path: Option<GroupPath>,
    /// How deep in the tree the header sits, counted from zero.
    pub depth: usize,
    /// How many hosts the current filters keep inside it and its children.
    pub hosts: usize,
    /// Whether its rows are drawn under it.
    pub expanded: bool,
}

/// One saved host of the flattened list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostRow {
    /// The host, by identifier: a row is never identified by its position (R18).
    pub id: HostId,
}

/// One row of the list the window draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostListRow {
    /// A group header, which opens and closes its branch.
    Group(HostGroupRow),
    /// A saved host.
    Host(HostRow),
}

/// The flattened list: the rows to draw and the hosts the filters keep.
#[derive(Clone, Debug, Default)]
pub struct HostList {
    rows: Vec<HostListRow>,
    matched: Vec<HostId>,
    all: Vec<HostId>,
}

impl HostList {
    /// The rows to draw, in order.
    #[must_use]
    pub fn rows(&self) -> &[HostListRow] {
        &self.rows
    }

    /// How many rows there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the list has no row at all.
    ///
    /// A closed group keeps its header, so this is the emptiness of what is drawn, not
    /// of what the filters kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many hosts survive the current search and filters.
    #[must_use]
    pub fn matched_len(&self) -> usize {
        self.matched.len()
    }

    /// How many hosts are saved.
    #[must_use]
    pub fn total(&self) -> usize {
        self.all.len()
    }

    /// The hosts one scope covers.
    #[must_use]
    pub fn ids_in_scope(&self, scope: SelectionScope) -> &[HostId] {
        match scope {
            SelectionScope::CurrentResults => &self.matched,
            SelectionScope::AllHosts => &self.all,
        }
    }
}

/// The hosts the user picked, by identifier (R18).
///
/// Identifiers survive a search, a filter, a collapse and a reload, so a selection made
/// in one view means the same hosts in the next one. Nothing here is positional, which
/// is what keeps a move from using the row numbers of a list that has since changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostSelection {
    ids: BTreeSet<HostId>,
}

impl HostSelection {
    /// Whether a host is selected.
    #[must_use]
    pub fn contains(&self, id: HostId) -> bool {
        self.ids.contains(&id)
    }

    /// Adds or removes one host; returns whether it is selected afterwards.
    pub fn toggle(&mut self, id: HostId) -> bool {
        if self.ids.remove(&id) {
            false
        } else {
            self.ids.insert(id);
            true
        }
    }

    /// Replaces the selection with the given hosts.
    pub fn replace(&mut self, ids: impl IntoIterator<Item = HostId>) {
        self.ids = ids.into_iter().collect();
    }

    /// Empties the selection.
    pub fn clear(&mut self) {
        self.ids.clear();
    }

    /// Drops every host that is not in the allowed list.
    pub fn retain(&mut self, allowed: &[HostId]) {
        self.ids.retain(|id| allowed.contains(id));
    }

    /// How many hosts are selected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The selected hosts, in identifier order, for a caller that has to send them on.
    #[must_use]
    pub fn ids(&self) -> Vec<HostId> {
        self.ids.iter().copied().collect()
    }
}

/// The group change a dialog is asking about (R18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupAction {
    /// Create a new group under the current one.
    Create,
    /// Rename a group and everything inside it.
    Rename(GroupPath),
    /// Move the selected hosts into a group.
    Move,
    /// Delete a group, first moving its hosts somewhere else.
    Delete(GroupPath),
}

impl GroupAction {
    /// Debug selector of the dialog, used by the tests.
    #[must_use]
    pub fn debug_selector(&self) -> &'static str {
        match self {
            Self::Create => "hosts.group.create",
            Self::Rename(_) => "hosts.group.rename",
            Self::Move => "hosts.group.move",
            Self::Delete(_) => "hosts.group.delete",
        }
    }
}

/// What a group dialog holds while it is open.
///
/// The name and the destination stay text until the tree accepts them, so a refused
/// path is shown in the field that produced it instead of being silently rewritten.
#[derive(Clone, Debug)]
pub struct GroupEditor {
    action: GroupAction,
    name: String,
    destination: String,
    error: Option<String>,
}

impl GroupEditor {
    /// A dialog for one change, with the name it starts from.
    #[must_use]
    pub fn new(action: GroupAction, name: impl Into<String>) -> Self {
        Self {
            action,
            name: name.into(),
            destination: String::new(),
            error: None,
        }
    }

    /// The change being asked about.
    #[must_use]
    pub fn action(&self) -> &GroupAction {
        &self.action
    }

    /// The group name as typed.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Replaces the typed name.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Where the hosts go: a group path, or blank for "no group".
    #[must_use]
    pub fn destination(&self) -> &str {
        &self.destination
    }

    /// Replaces the typed destination.
    pub fn set_destination(&mut self, destination: impl Into<String>) {
        self.destination = destination.into();
    }

    /// Why the tree refused the change.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Records a refusal.
    pub fn set_error(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
    }
}

/// A deterministic set of saved hosts, for the scale tests and the capture (R18).
///
/// Names, addresses, users and groups are derived from the position, so the same count
/// always produces the same list to search, filter and scroll. The identifiers are the
/// ones the domain generates: nothing here compares them across runs, and a capture that
/// needs a stable identifier builds its own records from the same fields.
#[cfg(test)]
#[must_use]
pub fn deterministic_hosts(count: usize) -> Vec<HostConfig> {
    (0..count)
        .map(|index| {
            let shape = scale_host_shape(index);
            let mut host = HostConfig::new(shape.name, shape.address, 22, shape.user)
                .expect("a valid fixture host");
            host.set_group_path(shape.group.and_then(|path| GroupPath::parse(path).ok()));
            host
        })
        .collect()
}

/// What one host of the scale fixture holds, at a position.
///
/// The name, the address, the user and the group all come from the position, so a test
/// and a capture describe the same five thousand hosts. The capture also writes the
/// identifier, because the list tints a row from it; the tests do not, because they never
/// compare identifiers across runs.
#[cfg(any(test, feature = "ui-capture"))]
#[derive(Clone, Debug)]
pub struct HostShape {
    /// The display name.
    pub name: String,
    /// The address.
    pub address: String,
    /// The login user.
    pub user: &'static str,
    /// The group path, when the host is in one.
    pub group: Option<&'static str>,
}

/// The shape of one host of the scale fixture.
#[cfg(any(test, feature = "ui-capture"))]
#[must_use]
pub fn scale_host_shape(index: usize) -> HostShape {
    HostShape {
        name: format!("node-{:04}", index + 1),
        address: format!(
            "10.{}.{}.{}",
            10 + (index / 1024) % 40,
            (index / 32) % 32,
            index % 32 + 1
        ),
        user: if index.is_multiple_of(5) {
            "ops"
        } else {
            "deploy"
        },
        group: match index % 5 {
            0 => Some("Prod/Web"),
            1 => Some("Prod/DB"),
            2 => Some("Prod/Edge"),
            3 => Some("Staging"),
            _ => None,
        },
    }
}

/// Reads a group path from what the user typed.
///
/// Blank means "no group": a destination of nothing is how a deletion says the hosts
/// stay saved and leave the tree. Anything else has to be a path the model accepts, and
/// the refusal is the model's own sentence, which names the segment to change.
///
/// # Errors
///
/// Returns the sentence of the path parser when the text is not a path.
pub fn group_path_from_text(text: &str) -> Result<Option<GroupPath>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    GroupPath::parse(text)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Every group path worth drawing: the saved tree, plus the parents of any path a host
/// names.
///
/// A host can carry a path whose parents were never saved by an older build or a hand
/// edit; drawing it under a parent that does not exist would hide it, so the parents are
/// added here instead of being invented again by the renderer.
fn drawn_paths(book: &HostBook, groups: &GroupBook) -> Vec<GroupPath> {
    let mut paths: BTreeSet<GroupPath> = groups.paths().iter().cloned().collect();
    for host in book.hosts() {
        let mut current = host.group_path();
        while let Some(path) = current {
            current = path.parent();
            paths.insert(path);
        }
    }
    paths.into_iter().collect()
}

/// The paths directly inside one parent, in order.
fn child_paths(paths: &[GroupPath], parent: Option<&GroupPath>) -> Vec<GroupPath> {
    paths
        .iter()
        .filter(|path| path.parent().as_ref() == parent)
        .cloned()
        .collect()
}

/// The kept hosts of one group and everything inside it, in saved order.
fn hosts_under(book: &HostBook, kept: &BTreeSet<HostId>, path: &GroupPath) -> Vec<HostId> {
    book.hosts()
        .iter()
        .filter(|host| kept.contains(&host.id()))
        .filter(|host| host.group_path().is_some_and(|host| host.starts_with(path)))
        .map(HostConfig::id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::copy::{CHINESE, ENGLISH};
    use shellune_storage::HostBook;

    fn host(name: &str, address: &str, port: u16, user: &str, group: Option<&str>) -> HostConfig {
        let mut host = HostConfig::new(name, address, port, user).expect("a valid host");
        host.set_group(group);
        host
    }

    #[test]
    fn the_connection_page_keeps_the_numbers_it_reads_back() {
        let defaults = ConnectionDefaults::new(45, 4, 12).expect("valid defaults");
        let draft = ConnectionDraft::of(defaults);
        assert_eq!(draft.resolve(), Ok(defaults));
        assert_eq!(draft.text(ConnectionField::KeepaliveInterval), "45");
        assert_eq!(draft.error(), None);
        assert_eq!(
            draft.detection_window_secs(),
            Some(180),
            "45 s times 4 probes is when the link is given up on"
        );
    }

    #[test]
    fn zero_switches_keepalive_off_and_is_not_a_refusal() {
        let mut draft = ConnectionDraft::of(ConnectionDefaults::default());
        assert!(draft.keepalive_on());
        draft.set_keepalive(false);
        assert_eq!(draft.text(ConnectionField::KeepaliveInterval), "0");
        assert!(!draft.keepalive_on());
        assert_eq!(draft.error(), None, "zero is a value, not a mistake");
        assert_eq!(draft.detection_window_secs(), None);
        let written = draft.resolve().expect("zero is a usable interval");
        assert_eq!(written.keepalive_interval_secs(), 0);
        assert!(written.policy().keepalive().is_none());

        // Switching it back on restores what the user last chose rather than the
        // documented default.
        let mut chosen = ConnectionDraft::of(ConnectionDefaults::new(90, 3, 20).expect("valid"));
        chosen.set_keepalive(false);
        chosen.set_keepalive(true);
        assert_eq!(chosen.text(ConnectionField::KeepaliveInterval), "90");
    }

    #[test]
    fn an_unusable_number_is_refused_in_its_own_field() {
        let mut draft = ConnectionDraft::of(ConnectionDefaults::default());
        draft.set_text(ConnectionField::KeepaliveMisses, "0");
        let (field, problem) = draft.error().expect("zero probes is refused");
        assert_eq!(field, ConnectionField::KeepaliveMisses);
        assert!(
            matches!(problem, ConnectionInputError::Refused(_)),
            "the domain refused the range, got {problem:?}"
        );
        assert_eq!(
            draft.text(ConnectionField::KeepaliveMisses),
            "0",
            "the refused text stays in the field the user typed it into"
        );
        assert!(
            draft.resolve().is_err(),
            "an unusable number is not written to the file"
        );

        draft.set_text(ConnectionField::KeepaliveMisses, "-3");
        let (field, problem) = draft.error().expect("a negative count is refused");
        assert_eq!(field, ConnectionField::KeepaliveMisses);
        assert_eq!(
            problem,
            &ConnectionInputError::Negative {
                text: "-3".to_owned()
            }
        );

        draft.set_text(ConnectionField::KeepaliveMisses, "soon");
        assert_eq!(
            draft.error().map(|(field, _)| field),
            Some(ConnectionField::KeepaliveMisses)
        );

        // A number above the supported interval is refused rather than clamped.
        draft.set_text(ConnectionField::KeepaliveInterval, "99999");
        assert_eq!(
            draft.error().map(|(field, _)| field),
            Some(ConnectionField::KeepaliveInterval)
        );

        // Fixing the numbers clears the reason, wherever it pointed.
        draft.set_text(ConnectionField::KeepaliveInterval, "30");
        draft.set_text(ConnectionField::KeepaliveMisses, "3");
        assert_eq!(draft.error(), None);
        assert_eq!(draft.resolve(), Ok(ConnectionDefaults::default()));
    }

    #[test]
    fn the_sentence_about_a_refusal_names_the_limit_in_both_languages() {
        let problem =
            ConnectionInputError::Refused(ConnectionValueError::KeepaliveMaxMissesOutOfRange {
                value: 0,
                max: MAX_KEEPALIVE_MAX_MISSES,
            });
        let english = connection_problem_text(&problem, crate::app::copy::Locale::English);
        assert!(english.contains("100"), "{english}");
        assert!(english.contains("dead link"), "{english}");
        let chinese = connection_problem_text(&problem, crate::app::copy::Locale::Chinese);
        assert!(chinese.contains("100"), "{chinese}");
        assert!(chinese.contains("判定断开"), "{chinese}");
    }

    #[test]
    fn a_host_says_inherit_off_or_its_own_numbers() {
        let defaults = ConnectionDefaults::new(45, 4, 12).expect("valid defaults");

        // A host saved before the choice existed inherits, and its form starts from the
        // numbers it would really be dialled with.
        let mut draft = HostDraft::create();
        draft.refresh_connection_defaults(defaults);
        assert_eq!(draft.connection().choice(), ConnectionChoice::Inherit);
        assert_eq!(
            draft.connection().text(ConnectionField::KeepaliveInterval),
            "45"
        );
        assert_eq!(draft.connection().resolve(), Ok(None));

        // Override states all three, and they are what the record keeps.
        let mut overriding = HostDraft::create();
        overriding.refresh_connection_defaults(defaults);
        overriding
            .connection_mut()
            .set_choice(ConnectionChoice::Override);
        overriding
            .connection_mut()
            .set_text(ConnectionField::KeepaliveInterval, "15");
        overriding
            .connection_mut()
            .set_text(ConnectionField::KeepaliveMisses, "2");
        overriding
            .connection_mut()
            .set_text(ConnectionField::ConnectTimeout, "9");
        assert_eq!(overriding.connection().detection_window_secs(), Some(30));
        let record = overriding
            .connection()
            .resolve()
            .expect("valid numbers")
            .expect("an override states a record");
        assert_eq!(record.keepalive_interval_secs(), Some(15));
        assert_eq!(record.keepalive_max_misses(), Some(2));
        assert_eq!(record.connect_timeout_secs(), Some(9));

        // Off is the switch, and it is not the same as inheriting.
        let mut off = HostDraft::create();
        off.refresh_connection_defaults(defaults);
        off.connection_mut().set_choice(ConnectionChoice::Off);
        let record = off
            .connection()
            .resolve()
            .expect("off needs no numbers")
            .expect("off is a record");
        assert_eq!(record.keepalive_interval_secs(), Some(0));
        assert!(
            defaults.policy_for(Some(record)).keepalive().is_none(),
            "the host is dialled without a probe even though the default has one"
        );
    }

    #[test]
    fn a_host_form_refuses_an_unusable_override_and_keeps_the_typed_text() {
        let mut draft = HostDraft::create();
        draft.set_text(HostField::Name, "web");
        draft.set_text(HostField::Address, "10.0.0.2");
        draft.set_text(HostField::User, "deploy");
        draft
            .connection_mut()
            .set_choice(ConnectionChoice::Override);
        draft
            .connection_mut()
            .set_text(ConnectionField::KeepaliveMisses, "0");
        assert_eq!(
            draft.connection().error().map(|(field, _)| field),
            Some(ConnectionField::KeepaliveMisses)
        );
        let refusal = draft.build(None).expect_err("the form is refused");
        assert!(refusal.contains("keepalive misses"), "{refusal}");
        assert_eq!(
            draft.connection().text(ConnectionField::KeepaliveMisses),
            "0",
            "nothing was corrected behind the user"
        );
    }

    #[test]
    fn a_saved_host_reopens_with_the_choice_it_was_stored_with() {
        let defaults = ConnectionDefaults::default();
        let mut host = host("web", "10.0.0.2", 22, "deploy", None);
        assert_eq!(
            HostDraft::edit(&host).connection().choice(),
            ConnectionChoice::Inherit,
            "a host with no record inherits"
        );

        host.set_connection(Some(HostConnection::keepalive_off()));
        let mut draft = HostDraft::edit(&host);
        draft.refresh_connection_defaults(defaults);
        assert_eq!(draft.connection().choice(), ConnectionChoice::Off);

        host.set_connection(Some(
            HostConnection::new(Some(20), Some(5), Some(8)).expect("valid"),
        ));
        let mut draft = HostDraft::edit(&host);
        draft.refresh_connection_defaults(defaults);
        assert_eq!(draft.connection().choice(), ConnectionChoice::Override);
        assert_eq!(
            draft.connection().text(ConnectionField::ConnectTimeout),
            "8"
        );

        // A record that states only one field is shown as an override with the others
        // resolved from the default, so saving it writes what the form shows.
        host.set_connection(Some(
            HostConnection::new(None, Some(6), None).expect("valid"),
        ));
        let mut partial = HostDraft::edit(&host);
        partial.refresh_connection_defaults(ConnectionDefaults::new(50, 3, 25).expect("valid"));
        assert_eq!(partial.connection().choice(), ConnectionChoice::Override);
        assert_eq!(
            partial
                .connection()
                .text(ConnectionField::KeepaliveInterval),
            "50"
        );
        assert_eq!(
            partial.connection().text(ConnectionField::KeepaliveMisses),
            "6"
        );
        assert_eq!(
            partial.connection().text(ConnectionField::ConnectTimeout),
            "25"
        );
    }

    #[test]
    fn the_copy_protocol_belongs_to_the_host_and_defaults_to_sftp() {
        let mut draft = HostDraft::create();
        assert_eq!(
            draft.protocol(),
            TransferProtocol::Sftp,
            "a new host copies over SFTP"
        );
        draft.set_text(HostField::Name, "web");
        draft.set_text(HostField::Address, "10.0.0.5");
        draft.set_text(HostField::User, "deploy");
        let edit = draft.build(None).expect("a valid host");
        assert_eq!(edit.host().transfer_protocol(), TransferProtocol::Sftp);
        assert!(
            !serde_json::to_string(edit.host())
                .expect("the host serializes")
                .contains("transfer_protocol"),
            "the default is not stored as a field, so an older record stays byte-identical"
        );

        // Choosing SCP travels with the host, so the next connection starts there.
        draft.set_protocol(TransferProtocol::Scp);
        let edit = draft.build(None).expect("a valid host");
        assert_eq!(edit.host().transfer_protocol(), TransferProtocol::Scp);
        let reopened = HostDraft::edit(edit.host());
        assert_eq!(
            reopened.protocol(),
            TransferProtocol::Scp,
            "editing a host shows the protocol it will copy with"
        );

        // Choosing the default again removes the field instead of encoding it twice.
        let mut reopened = reopened;
        assert!(reopened.set_protocol(TransferProtocol::Sftp));
        assert!(!reopened.set_protocol(TransferProtocol::Sftp));
        let saved = reopened.build(None).expect("a valid host");
        assert_eq!(saved.host().transfer_protocol(), TransferProtocol::Sftp);
        assert!(
            !serde_json::to_string(saved.host())
                .expect("the host serializes")
                .contains("transfer_protocol")
        );
    }

    #[test]
    fn a_new_form_starts_with_the_default_port_and_no_content() {
        let draft = HostDraft::create();
        assert_eq!(draft.text(HostField::Port), "22");
        assert_eq!(draft.text(HostField::Name), "");
        assert_eq!(draft.editing(), None);
        assert_eq!(
            draft.build(None),
            Err("host name cannot be empty".to_owned())
        );
    }

    #[test]
    fn a_port_is_refused_by_the_domain_when_it_is_outside_the_tcp_range() {
        let mut draft = HostDraft::create();
        draft.set_text(HostField::Name, "web");
        draft.set_text(HostField::Address, "10.0.0.5");
        draft.set_text(HostField::User, "deploy");

        draft.set_text(HostField::Port, "0");
        let error = draft.build(None).expect_err("port 0 is not usable");
        assert!(
            error.contains("1-65535"),
            "the reason names the range: {error}"
        );

        draft.set_text(HostField::Port, "70000");
        let error = draft.build(None).expect_err("port 70000 is not usable");
        assert!(
            error.contains("70000"),
            "the reason names the value: {error}"
        );

        draft.set_text(HostField::Port, "ssh");
        let error = draft.build(None).expect_err("a word is not a port");
        assert!(error.contains("ssh"), "the reason names the value: {error}");

        draft.set_text(HostField::Port, "2222");
        let edit = draft.build(None).expect("a valid host");
        assert_eq!(edit.host().port().get(), 2222);
        assert_eq!(edit.host().target(), "deploy@10.0.0.5:2222");
        assert_eq!(edit.replaces(), None);
    }

    #[test]
    fn an_empty_port_field_means_the_ssh_default() {
        let mut draft = HostDraft::create();
        draft.set_text(HostField::Name, "web");
        draft.set_text(HostField::Address, "10.0.0.5");
        draft.set_text(HostField::User, "deploy");
        draft.set_text(HostField::Port, "   ");
        let edit = draft.build(None).expect("an empty port is the default");
        assert_eq!(edit.host().port().get(), 22);
    }

    #[test]
    fn a_blank_group_is_no_group_and_an_edit_keeps_the_identifier() {
        let saved = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
        let mut draft = HostDraft::edit(&saved);
        assert_eq!(draft.editing(), Some(saved.id()));
        assert_eq!(draft.text(HostField::Group), "Prod");

        draft.set_text(HostField::Group, "   ");
        draft.set_text(HostField::Address, "web.example.org");
        let edit = draft.build(Some(&saved)).expect("a valid edit");
        assert_eq!(edit.host().id(), saved.id(), "an edit keeps the identifier");
        assert_eq!(edit.replaces(), Some(saved.id()));
        assert_eq!(edit.host().group(), None);
        assert_eq!(edit.host().address(), "web.example.org");
    }

    #[test]
    fn a_rejected_edit_leaves_the_saved_host_alone() {
        let saved = host("web", "10.0.0.5", 22, "deploy", None);
        let mut draft = HostDraft::edit(&saved);
        draft.set_text(HostField::Port, "0");
        assert!(draft.build(Some(&saved)).is_err());
        assert_eq!(saved.port().get(), 22);
        assert_eq!(
            draft.text(HostField::Port),
            "0",
            "the field keeps what was typed"
        );
    }

    #[test]
    fn the_views_read_the_same_records_and_the_search_filters_all_of_them() {
        let web = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
        let api = host("api", "api.example.org", 2222, "svc", Some("prod"));
        let lab = host("lab", "lab.internal", 22, "root", None);
        let mut book = HostBook::from_hosts(vec![web.clone(), api, lab]).expect("distinct hosts");
        book.record_connection(web.id(), 1_700_000_000)
            .expect("recorded");

        // No window holds a session here, so the status filter has nothing to find.
        let no_sessions = |_: HostId| false;
        let groups = GroupBook::new();

        let mut panel = HostsPanel::new();
        assert_eq!(panel.view(), HostView::All);
        assert_eq!(panel.list(&book, &groups, &no_sessions).matched_len(), 3);
        assert_eq!(
            panel.list(&book, &groups, &no_sessions).rows().len(),
            3,
            "the flat view draws one row per host"
        );

        panel.select_view(HostView::Recent);
        assert_eq!(
            panel.list(&book, &groups, &no_sessions).rows().len(),
            1,
            "only a real connection is recent"
        );

        panel.set_search("prod");
        assert_eq!(
            panel.list(&book, &groups, &no_sessions).matched_len(),
            1,
            "the recent view is a filter too: current results name only the rows it shows"
        );
        assert_eq!(
            panel.list(&book, &groups, &no_sessions).rows().len(),
            1,
            "the recent view applies the same search"
        );

        panel.set_search("nothing matches this");
        assert_eq!(panel.list(&book, &groups, &no_sessions).matched_len(), 0);
        assert!(panel.list(&book, &groups, &no_sessions).is_empty());
    }

    #[test]
    fn a_host_that_turns_probing_off_but_keeps_its_timeout_stays_an_override() {
        let mut record = HostConnection::keepalive_off();
        record
            .set_connect_timeout_secs(Some(45))
            .expect("a stated timeout");
        let draft = HostConnectionDraft::of(Some(record), ConnectionDefaults::default());
        assert_eq!(
            draft.choice(),
            ConnectionChoice::Override,
            "turning probing off is not the same record as stating a timeout"
        );
        let kept = draft
            .resolve()
            .expect("the stated numbers are valid")
            .expect("the host states them");
        assert_eq!(kept.keepalive_interval_secs(), Some(0));
        assert_eq!(kept.connect_timeout_secs(), Some(45));
    }

    #[test]
    fn the_host_form_keeps_a_nested_group_path_and_a_blank_field_clears_it() {
        let mut saved = host("web", "10.0.0.5", 22, "deploy", None);
        saved.set_group_path(Some(GroupPath::parse("Prod/Web").expect("a path")));

        let mut draft = HostDraft::edit(&saved);
        assert_eq!(draft.group, "Prod/Web", "the form shows the path it stores");
        let edit = draft.build(Some(&saved)).expect("the host is valid");
        assert_eq!(
            edit.host().group_path().map(|path| path.to_string()),
            Some("Prod/Web".to_owned()),
            "saving the form must not flatten the host into a look-alike group"
        );

        draft.group.clear();
        let cleared = draft.build(Some(&saved)).expect("the host is still valid");
        assert_eq!(
            cleared.host().group_path(),
            None,
            "a blank field takes the host out of its group"
        );
    }

    #[test]
    fn opening_a_form_clears_a_previous_message_and_question() {
        let saved = host("web", "10.0.0.5", 22, "deploy", None);
        let mut panel = HostsPanel::new();
        panel.set_message("Host saved.", false);
        panel.ask_delete(saved.id());
        assert_eq!(panel.pending_delete(), Some(saved.id()));

        panel.open_create();
        assert!(panel.is_editing());
        assert_eq!(panel.pending_delete(), None);
        assert!(panel.message().is_none());

        panel.open_edit(&saved);
        assert_eq!(panel.draft().and_then(HostDraft::editing), Some(saved.id()));
        panel.close_editor();
        assert!(!panel.is_editing());
    }

    #[test]
    fn switching_the_view_drops_a_question_about_the_previous_one() {
        let saved = host("web", "10.0.0.5", 22, "deploy", None);
        let mut panel = HostsPanel::new();
        panel.ask_delete(saved.id());
        panel.select_view(HostView::Recent);
        assert_eq!(panel.view(), HostView::Recent);
        assert_eq!(panel.pending_delete(), None);
    }

    #[test]
    fn unix_seconds_are_shown_as_a_readable_utc_instant() {
        assert_eq!(format_unix(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_unix(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(format_unix(1_700_000_100), "2023-11-14 22:15 UTC");
        assert_eq!(format_unix(951_782_400), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn the_import_panel_drops_a_result_that_belongs_to_another_path() {
        let mut panel = ImportPanel::new();
        assert!(!panel.is_open());
        assert_eq!(panel.path(), DEFAULT_CONFIG_TEXT);
        assert!(panel.preview().is_none());
        assert!(panel.error().is_none());

        panel.open("C:/Users/example/.ssh/config");
        assert!(panel.is_open());
        assert_eq!(panel.path(), "C:/Users/example/.ssh/config");

        let preview = ImportPreview::from_text(
            "C:/Users/example/.ssh/config",
            "Host web\n    HostName web.example.org\n    User deploy\n",
        )
        .expect("the file parses");
        panel.set_preview(preview);
        assert!(panel.preview().is_some());
        assert!(panel.error().is_none());

        // Editing the path is the start of another question, so the answer to the old
        // one must not stay on screen where it could be applied to the new file.
        panel.set_path("C:/Users/example/other-config");
        assert!(panel.preview().is_none());
        assert!(panel.error().is_none());

        panel.set_error("the file could not be read");
        assert!(panel.error().is_some());
        assert!(panel.preview().is_none());

        panel.close();
        assert!(!panel.is_open());
        assert!(panel.preview().is_none());
        assert_eq!(
            panel.path(),
            "C:/Users/example/other-config",
            "the path is kept for the next time the panel opens"
        );
    }

    #[test]
    fn an_import_message_states_what_was_written_and_what_was_not() {
        let (message, failed) = import_message(&ENGLISH, 2, 1, 3, &[]);
        assert!(!failed);
        assert!(message.contains('2'), "{message}");
        assert!(message.contains("Imported"), "{message}");
        assert!(message.contains("need review"), "{message}");
        assert!(message.contains("skipped"), "{message}");

        let (chinese, chinese_failed) = import_message(&CHINESE, 2, 1, 3, &[]);
        assert!(!chinese_failed);
        assert!(chinese.contains("已导入"), "{chinese}");
        assert!(chinese.contains("需复核"), "{chinese}");

        let withheld = vec![(
            "through-bastion".to_owned(),
            "ProxyJump was not applied".to_owned(),
        )];
        let (partial, partial_failed) = import_message(&ENGLISH, 2, 1, 0, &withheld);
        assert!(partial_failed);
        assert!(partial.contains("through-bastion"), "{partial}");
        assert!(partial.contains("not imported"), "{partial}");
        assert!(partial.contains("ProxyJump was not applied"), "{partial}");
    }

    #[test]
    fn the_grouped_rows_hold_every_host_once_under_its_own_group() {
        // The rows are flattened from group headers and hosts; a host that appears twice,
        // or not at all, is a list that disagrees with the file it was read from.
        let web = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
        let api = host("api", "api.example.org", 2222, "svc", Some("Prod"));
        let lab = host("lab", "lab.internal", 22, "root", None);
        let mut book = HostBook::from_hosts(vec![web.clone(), api.clone(), lab.clone()])
            .expect("distinct hosts");
        let mut groups = GroupBook::new();
        groups
            .insert(&GroupPath::parse("Prod").expect("a valid path"))
            .expect("the tree accepts it");

        let no_sessions = |_: HostId| false;
        let mut panel = HostsPanel::new();
        panel.select_view(HostView::Groups);
        let list = panel.list(&book, &groups, &no_sessions);
        let hosts: Vec<HostId> = list
            .rows()
            .iter()
            .filter_map(|row| match row {
                HostListRow::Host(host) => Some(host.id),
                HostListRow::Group(_) => None,
            })
            .collect();
        assert_eq!(
            hosts,
            vec![web.id(), api.id(), lab.id()],
            "every host is listed once, in saved order, under its own header"
        );
        let headers: Vec<Option<GroupPath>> = list
            .rows()
            .iter()
            .filter_map(|row| match row {
                HostListRow::Group(group) => Some(group.path.clone()),
                HostListRow::Host(_) => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![Some(GroupPath::parse("Prod").expect("a valid path")), None,],
            "the named group and the hosts without one each get a header"
        );

        // Closing a group hides its rows and keeps its hosts in the result: the count in
        // the header and the set a selection covers are the same either way.
        panel.toggle_group(&GroupPath::parse("Prod").expect("a valid path"));
        let closed = panel.list(&book, &groups, &no_sessions);
        assert_eq!(closed.matched_len(), 3);
        assert_eq!(
            closed.rows().len(),
            3,
            "two headers and the ungrouped host, without the closed group's hosts"
        );

        // A host that is edited away leaves the list with it.
        book.remove(lab.id()).expect("the host is saved");
        assert_eq!(panel.list(&book, &groups, &no_sessions).matched_len(), 2);
    }

    #[test]
    fn a_draft_keeps_an_ordered_jump_chain_and_clears_a_blank_proxy_command() {
        let bastion = host("bastion", "10.0.0.1", 22, "jump", None);
        let second = host("edge", "10.0.0.3", 22, "jump", None);
        let web = host("web", "10.0.0.2", 22, "deploy", None);
        let mut draft = HostDraft::edit(&web);
        assert!(draft.jumps().is_empty());
        assert_eq!(draft.text(HostField::ProxyCommand), "");
        assert!(draft.add_jump(bastion.id()));
        assert!(draft.add_jump(second.id()));
        assert!(!draft.add_jump(web.id()), "a host is not a jump to itself");
        assert!(!draft.add_jump(bastion.id()), "a hop is listed once");
        draft.move_jump(1, true);
        assert_eq!(draft.jumps(), &[second.id(), bastion.id()]);
        draft.set_text(HostField::ProxyCommand, "  nc %h %p  ");
        assert!(draft.proxy_conflicts());
        let edit = draft.build(Some(&web)).expect("the host is valid");
        let saved = edit.host();
        assert_eq!(saved.jumps(), &[second.id(), bastion.id()]);
        assert_eq!(saved.proxy_command(), Some("nc %h %p"));

        let mut again = HostDraft::edit(saved);
        assert_eq!(again.jumps(), &[second.id(), bastion.id()]);
        again.remove_jump(0);
        again.set_text(HostField::ProxyCommand, "   ");
        let cleared = again
            .build(Some(saved))
            .expect("clearing the command is valid");
        assert_eq!(cleared.host().jumps(), &[bastion.id()]);
        assert_eq!(cleared.host().proxy_command(), None);
        assert!(!HostDraft::edit(cleared.host()).proxy_conflicts());
    }
}
