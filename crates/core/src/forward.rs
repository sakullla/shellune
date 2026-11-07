//! Saved local, remote and dynamic forwarding rules (R11).
//!
//! A rule is configuration. It records which saved host would carry the tunnel and
//! which endpoints it would use. It does not bind a port, open a socket or start a
//! connection: those are later, explicit actions. Restoring a rule therefore cannot
//! be mistaken for a listener that is already up.

use serde::{Deserialize, Serialize};

use crate::host::{HostValidationError, Port};
use crate::ids::{ForwardId, HostId};

/// The format this build writes for the forwarding document.
pub const FORWARD_FORMAT_VERSION: u32 = 1;

/// Which tunnel a rule describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// Local port to a target reached through the SSH host.
    Local,
    /// Remote port on the SSH host to a target reached from this machine.
    Remote,
    /// Local SOCKS port. The client chooses the target per connection.
    Dynamic,
}

impl ForwardKind {
    /// Short name used in errors.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Dynamic => "dynamic",
        }
    }
}

/// Why a forwarding endpoint or rule shape was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForwardBuildError {
    /// The address is empty or only whitespace.
    #[error("forwarding address cannot be empty")]
    EmptyAddress,
    /// The address contains whitespace and is not one host.
    #[error("forwarding address cannot contain whitespace")]
    AddressContainsWhitespace,
    /// The port falls outside `1..=65535`.
    #[error("port {value} is outside the valid range 1-65535")]
    PortOutOfRange {
        /// The rejected value.
        value: u32,
    },
    /// A local or remote rule was stored without somewhere to forward to.
    #[error(
        "forward rule {id} is {kind} and needs a target host and port; add both or change the kind. The rule was not started."
    )]
    MissingTarget {
        /// The rule that is incomplete.
        id: ForwardId,
        /// Which kind was declared.
        kind: &'static str,
    },
    /// A dynamic rule was stored with a target, which a SOCKS listener does not have.
    #[error(
        "dynamic forward rule {id} has a target, but a dynamic rule only listens; remove the target. The rule was not started."
    )]
    UnexpectedTarget {
        /// The rule that carries the extra target.
        id: ForwardId,
    },
}

impl From<HostValidationError> for ForwardBuildError {
    fn from(error: HostValidationError) -> Self {
        match error {
            HostValidationError::PortOutOfRange { value } => Self::PortOutOfRange { value },
            // Endpoint construction only asks `Port` for a port check.
            _ => Self::PortOutOfRange { value: 0 },
        }
    }
}

/// One side of a forwarding rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EndpointWire", into = "EndpointWire")]
pub struct Endpoint {
    host: String,
    port: Port,
}

/// Stored shape of an endpoint. Unknown fields are rejected so a secret cannot ride along.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointWire {
    host: String,
    port: Port,
}

impl TryFrom<EndpointWire> for Endpoint {
    type Error = ForwardBuildError;

    fn try_from(wire: EndpointWire) -> Result<Self, Self::Error> {
        Self::new(wire.host, wire.port.get())
    }
}

impl From<Endpoint> for EndpointWire {
    fn from(endpoint: Endpoint) -> Self {
        Self {
            host: endpoint.host,
            port: endpoint.port,
        }
    }
}

impl Endpoint {
    /// Validates an address and a TCP port.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardBuildError`] when the address is blank or contains
    /// whitespace, or the port is outside `1..=65535`.
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, ForwardBuildError> {
        let host = host.into();
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(ForwardBuildError::EmptyAddress);
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(ForwardBuildError::AddressContainsWhitespace);
        }
        let port = Port::new(u32::from(port))?;
        Ok(Self {
            host: trimmed.to_owned(),
            port,
        })
    }

    /// A loopback endpoint, the default place a local listener binds.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardBuildError::PortOutOfRange`] when `port` is `0`.
    pub fn loopback(port: u16) -> Result<Self, ForwardBuildError> {
        Self::new("127.0.0.1", port)
    }

    /// Host name or address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// TCP port.
    #[must_use]
    pub fn port(&self) -> Port {
        self.port
    }

    /// Whether this address is one of the loopback spellings this build treats as local.
    ///
    /// Other spellings are treated as exposed, so a later start has to ask before binding
    /// them. This method does not bind anything.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        matches!(
            self.host.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1" | "[::1]"
        )
    }
}

/// One saved forwarding rule.
///
/// Fields stay private so a dynamic rule cannot also carry a target, and so a
/// document that smuggles in a password field is rejected rather than stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ForwardRuleWire")]
pub struct ForwardRule {
    id: ForwardId,
    host: HostId,
    kind: ForwardKind,
    listen: Endpoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<Endpoint>,
}

/// Stored shape of a rule, checked by [`ForwardRule`]'s constructor rules.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardRuleWire {
    id: ForwardId,
    host: HostId,
    kind: ForwardKind,
    listen: Endpoint,
    #[serde(default)]
    target: Option<Endpoint>,
}

impl TryFrom<ForwardRuleWire> for ForwardRule {
    type Error = ForwardBuildError;

    fn try_from(wire: ForwardRuleWire) -> Result<Self, Self::Error> {
        match wire.kind {
            ForwardKind::Dynamic => {
                if wire.target.is_some() {
                    return Err(ForwardBuildError::UnexpectedTarget { id: wire.id });
                }
                Ok(Self::from_parts(
                    wire.id,
                    wire.host,
                    ForwardKind::Dynamic,
                    wire.listen,
                    None,
                ))
            }
            ForwardKind::Local | ForwardKind::Remote => {
                let Some(target) = wire.target else {
                    return Err(ForwardBuildError::MissingTarget {
                        id: wire.id,
                        kind: wire.kind.label(),
                    });
                };
                Ok(Self::from_parts(
                    wire.id,
                    wire.host,
                    wire.kind,
                    wire.listen,
                    Some(target),
                ))
            }
        }
    }
}

impl ForwardRule {
    fn from_parts(
        id: ForwardId,
        host: HostId,
        kind: ForwardKind,
        listen: Endpoint,
        target: Option<Endpoint>,
    ) -> Self {
        Self {
            id,
            host,
            kind,
            listen,
            target,
        }
    }

    /// A local forward. The listen address is whatever the caller passes; loopback
    /// is only the default helper, not a silent rewrite.
    #[must_use]
    pub fn local(host: HostId, listen: Endpoint, target: Endpoint) -> Self {
        Self::from_parts(
            ForwardId::new(),
            host,
            ForwardKind::Local,
            listen,
            Some(target),
        )
    }

    /// A remote forward. `listen` is the address on the SSH host.
    #[must_use]
    pub fn remote(host: HostId, listen: Endpoint, target: Endpoint) -> Self {
        Self::from_parts(
            ForwardId::new(),
            host,
            ForwardKind::Remote,
            listen,
            Some(target),
        )
    }

    /// A dynamic SOCKS forward. There is no fixed target.
    #[must_use]
    pub fn dynamic(host: HostId, listen: Endpoint) -> Self {
        Self::from_parts(ForwardId::new(), host, ForwardKind::Dynamic, listen, None)
    }

    /// Stable id.
    #[must_use]
    pub fn id(&self) -> ForwardId {
        self.id
    }

    /// The saved host that would carry the tunnel. Not a live session.
    #[must_use]
    pub fn host(&self) -> HostId {
        self.host
    }

    /// Which tunnel this rule describes.
    #[must_use]
    pub fn kind(&self) -> ForwardKind {
        self.kind
    }

    /// The listening endpoint. Owning it does not mean a socket is open.
    #[must_use]
    pub fn listen(&self) -> &Endpoint {
        &self.listen
    }

    /// Where traffic goes, or `None` for a dynamic rule.
    #[must_use]
    pub fn target(&self) -> Option<&Endpoint> {
        self.target.as_ref()
    }

    /// Whether a later start must ask before binding a non-loopback local port.
    ///
    /// Remote listens are on the server, so they are not a local exposure decision.
    /// The answer is derived from the saved address; nothing is bound here.
    #[must_use]
    pub fn needs_exposure_confirmation(&self) -> bool {
        matches!(self.kind, ForwardKind::Local | ForwardKind::Dynamic) && !self.listen.is_loopback()
    }
}

/// Why one saved rule cannot be used until its host reference is fixed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForwardFault {
    /// The rule names a host id that is not in the host book.
    #[error("{message}")]
    MissingHost {
        /// The rule that needs a decision.
        rule: ForwardId,
        /// The host id that is not saved.
        host: HostId,
        /// What to do next. The rule is still saved and was not started.
        message: String,
    },
}

impl ForwardFault {
    /// The explanation a person can act on.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::MissingHost { message, .. } => message,
        }
    }
}

/// The forwarding rules a user saved.
///
/// The document has a version so a newer build is refused by name instead of being
/// partially read. It has no field for a live listener, a socket or a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardBook {
    version: u32,
    rules: Vec<ForwardRule>,
}

impl Default for ForwardBook {
    fn default() -> Self {
        Self {
            version: FORWARD_FORMAT_VERSION,
            rules: Vec::new(),
        }
    }
}

impl ForwardBook {
    /// An empty book at the current format version.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The format version the records were read from.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Saved rules, in saved order.
    #[must_use]
    pub fn rules(&self) -> &[ForwardRule] {
        &self.rules
    }

    /// One rule by id.
    #[must_use]
    pub fn rule(&self, id: ForwardId) -> Option<&ForwardRule> {
        self.rules.iter().find(|rule| rule.id() == id)
    }

    /// Adds a rule.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardBookError::DuplicateRule`] when the id is already saved.
    /// A missing host is not an error here: the rule is kept and reported by
    /// [`Self::faults`].
    pub fn insert(&mut self, rule: ForwardRule) -> Result<(), ForwardBookError> {
        if self.rule(rule.id()).is_some() {
            return Err(ForwardBookError::DuplicateRule { id: rule.id() });
        }
        self.rules.push(rule);
        Ok(())
    }

    /// Replaces a saved rule, keeping its position.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardBookError::UnknownRule`] when the id is not saved.
    pub fn update(&mut self, rule: ForwardRule) -> Result<(), ForwardBookError> {
        let Some(slot) = self.rules.iter_mut().find(|saved| saved.id() == rule.id()) else {
            return Err(ForwardBookError::UnknownRule { id: rule.id() });
        };
        *slot = rule;
        Ok(())
    }

    /// Removes a rule.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardBookError::UnknownRule`] when the id is not saved.
    pub fn remove(&mut self, id: ForwardId) -> Result<ForwardRule, ForwardBookError> {
        let Some(index) = self.rules.iter().position(|rule| rule.id() == id) else {
            return Err(ForwardBookError::UnknownRule { id });
        };
        Ok(self.rules.remove(index))
    }

    /// Rules whose host is not saved.
    ///
    /// Each fault names one rule. The book is not dropped, and no rule is started.
    #[must_use]
    pub fn faults(&self, mut host_exists: impl FnMut(HostId) -> bool) -> Vec<ForwardFault> {
        self.rules
            .iter()
            .filter(|rule| !host_exists(rule.host()))
            .map(|rule| {
                let host = rule.host();
                ForwardFault::MissingHost {
                    rule: rule.id(),
                    host,
                    message: format!(
                        "Forwarding rule {} references host {host}, which is not saved. Choose a saved host or remove the rule. The rule was not started.",
                        rule.id()
                    ),
                }
            })
            .collect()
    }

    /// Checks the invariants a stored document must satisfy.
    ///
    /// A missing host reference is not one of them: that is a local fault from
    /// [`Self::faults`], and the rule is still the rule the user saved.
    ///
    /// # Errors
    ///
    /// Returns the reason the document cannot be used, naming the duplicate id
    /// when two rules share one.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != FORWARD_FORMAT_VERSION {
            return Err(format!(
                "format version {} is not the version this build writes ({FORWARD_FORMAT_VERSION})",
                self.version
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for rule in &self.rules {
            if !seen.insert(rule.id()) {
                return Err(format!(
                    "two forwarding rules share the id {}. Remove the duplicate. No rule was started.",
                    rule.id()
                ));
            }
        }
        Ok(())
    }
}

/// Why a change to the in-memory forwarding book was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForwardBookError {
    /// A rule with this id is already saved.
    #[error("a forwarding rule with id {id} is already saved")]
    DuplicateRule {
        /// The id that collided.
        id: ForwardId,
    },
    /// The rule to change does not exist.
    #[error("no saved forwarding rule with id {id} exists")]
    UnknownRule {
        /// The id that was not found.
        id: ForwardId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen(port: u16) -> Endpoint {
        Endpoint::loopback(port).expect("loopback port")
    }

    #[test]
    fn rules_round_trip_and_have_nowhere_to_put_a_secret() {
        let host = HostId::new();
        let local = ForwardRule::local(
            host,
            listen(8080),
            Endpoint::new("db.internal", 5432).expect("target"),
        );
        let dynamic = ForwardRule::dynamic(host, listen(1080));
        let mut book = ForwardBook::new();
        book.insert(local.clone()).expect("insert");
        book.insert(dynamic.clone()).expect("insert");

        let json = serde_json::to_string(&book).expect("serializes");
        let secret = "hunter2-forward";
        assert!(!json.contains(secret));
        assert!(!json.contains("password"));
        assert!(!json.contains("listening"));
        let back: ForwardBook = serde_json::from_str(&json).expect("reads");
        assert_eq!(back, book);
        assert_eq!(back.rule(dynamic.id()).expect("dynamic").target(), None);
        assert!(!local.needs_exposure_confirmation());
        let exposed = ForwardRule::local(
            host,
            Endpoint::new("0.0.0.0", 8080).expect("exposed"),
            Endpoint::new("db.internal", 5432).expect("target"),
        );
        assert!(exposed.needs_exposure_confirmation());
        assert!(
            !ForwardRule::remote(
                host,
                Endpoint::new("0.0.0.0", 8080).expect("remote listen"),
                listen(80),
            )
            .needs_exposure_confirmation()
        );
    }

    #[test]
    fn a_dynamic_rule_with_a_target_and_a_local_rule_without_one_are_refused() {
        let id = ForwardId::new();
        let host = HostId::new();
        let dynamic = format!(
            r#"{{"id":"{id}","host":"{host}","kind":"dynamic","listen":{{"host":"127.0.0.1","port":1080}},"target":{{"host":"db","port":1}}}}"#
        );
        let error =
            serde_json::from_str::<ForwardRule>(&dynamic).expect_err("dynamic has no target");
        assert!(error.to_string().contains("remove the target"), "{error}");
        assert!(!error.to_string().contains("hunter2"));

        let local = format!(
            r#"{{"id":"{id}","host":"{host}","kind":"local","listen":{{"host":"127.0.0.1","port":8080}}}}"#
        );
        let error = serde_json::from_str::<ForwardRule>(&local).expect_err("local needs a target");
        assert!(error.to_string().contains("needs a target"), "{error}");
    }

    #[test]
    fn a_missing_host_is_one_rule_and_the_other_rule_stays_usable() {
        let saved = HostId::new();
        let gone = HostId::new();
        let mut book = ForwardBook::new();
        let keep = ForwardRule::dynamic(saved, listen(1080));
        let broken = ForwardRule::local(
            gone,
            listen(8080),
            Endpoint::new("db.internal", 5432).expect("target"),
        );
        book.insert(keep.clone()).expect("insert");
        book.insert(broken.clone()).expect("insert");
        let faults = book.faults(|id| id == saved);
        assert_eq!(faults.len(), 1);
        match &faults[0] {
            ForwardFault::MissingHost {
                rule,
                host,
                message,
            } => {
                assert_eq!(*rule, broken.id());
                assert_eq!(*host, gone);
                assert!(message.contains("not saved"));
                assert!(message.contains("not started"));
                assert!(!message.contains("hunter2"));
            }
        }
        assert!(book.rule(keep.id()).is_some());
    }
}
