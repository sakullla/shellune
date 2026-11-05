//! Validated SSH host configuration (R3).
//!
//! A [`HostConfig`] can only exist with a non-empty name, address and user plus a
//! port in `1..=65535`, whether it was built in code, edited later or read back from
//! storage. Saving a host never implies a connection was made.

use serde::{Deserialize, Serialize};

use crate::ids::HostId;

/// Port used when the user does not specify one.
pub const DEFAULT_SSH_PORT: u16 = 22;

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
}

#[derive(Deserialize)]
struct HostConfigWire {
    id: HostId,
    name: String,
    address: String,
    port: Port,
    user: String,
    group: Option<String>,
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
        Ok(Self {
            id: wire.id,
            name,
            address,
            port,
            user,
            group: wire
                .group
                .map(|group| group.trim().to_owned())
                .filter(|group| !group.is_empty()),
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

    /// Optional grouping label.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
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

    /// Assigns or clears the grouping label.
    pub fn set_group(&mut self, group: Option<impl Into<String>>) {
        self.group = group
            .map(Into::into)
            .map(|group: String| group.trim().to_owned())
            .filter(|group| !group.is_empty());
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
}
