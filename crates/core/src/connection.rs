//! Connection behaviour that is chosen rather than fixed (R20): the global
//! defaults, one host's own overrides, and the policy a connection is dialled with.
//!
//! Three facts decide the shape of this module:
//!
//! * keepalive is the encrypted-channel probe OpenSSH spells
//!   `ServerAliveInterval`/`ServerAliveCountMax`: after the configured silence the
//!   client sends a probe, and after that many unanswered probes the connection is
//!   treated as dead. The library also has an inactivity timeout, which closes a
//!   session that is merely quiet; a healthy idle session must not be killed for
//!   having nothing to say, so [`ConnectionPolicy::inactivity_timeout`] is always
//!   `None`.
//! * a host says one of three things: nothing at all (inherit the global default),
//!   that keepalive is off, or its own numbers. "Off" and "never asked" are kept
//!   apart because they mean opposite things: an explicit off must survive a change
//!   to the global default.
//! * every input is validated where it enters, and an unusable number is refused
//!   with its reason instead of being clamped to something nearby. Interval `0`
//!   means off; a probe count of `0` would mean no reply ever counts as death, so it
//!   is refused rather than stored.
//!
//! Interval `0` and "off" are the same thing on purpose: OpenSSH documents
//! `ServerAliveInterval 0` as disabling the probe, and an imported `0` therefore
//! becomes an explicit disable instead of being read as a silent default.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How often a live connection is probed when the user changes nothing.
pub const DEFAULT_KEEPALIVE_INTERVAL_SECS: u32 = 30;

/// How many probes may go unanswered before the connection is treated as dead.
pub const DEFAULT_KEEPALIVE_MAX_MISSES: u32 = 3;

/// How long the machine part of a connection attempt may take by default.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u32 = 20;

/// Largest keepalive interval this build stores.
///
/// An interval beyond this is not a keepalive, it is a promise not to notice a dead
/// network for an hour; `0` is how keepalive is turned off.
pub const MAX_KEEPALIVE_INTERVAL_SECS: u32 = 3_600;

/// Largest number of unanswered probes this build stores.
pub const MAX_KEEPALIVE_MAX_MISSES: u32 = 100;

/// Longest connect timeout this build stores.
pub const MAX_CONNECT_TIMEOUT_SECS: u32 = 600;

/// Why a connection value was refused.
///
/// Each variant carries the value that was rejected so the message can be exact and
/// the caller can show the field the user has to fix.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionValueError {
    /// The interval is above [`MAX_KEEPALIVE_INTERVAL_SECS`].
    #[error(
        "keepalive interval {value}s is above the {max}s Shellune supports; use 0 to turn keepalive off"
    )]
    KeepaliveIntervalOutOfRange {
        /// The rejected value, in seconds.
        value: u32,
        /// The largest value this build stores.
        max: u32,
    },
    /// The probe count is `0` or above [`MAX_KEEPALIVE_MAX_MISSES`].
    #[error(
        "keepalive stops after {value} unanswered probes, which is outside 1-{max}; 0 would never treat a silent connection as dead"
    )]
    KeepaliveMaxMissesOutOfRange {
        /// The rejected value.
        value: u32,
        /// The largest value this build stores.
        max: u32,
    },
    /// The connect timeout is `0` or above [`MAX_CONNECT_TIMEOUT_SECS`].
    #[error(
        "connect timeout {value}s is outside 1-{max}; 0 would leave an attempt without a deadline"
    )]
    ConnectTimeoutOutOfRange {
        /// The rejected value, in seconds.
        value: u32,
        /// The largest value this build stores.
        max: u32,
    },
}

fn check_interval(value: u32) -> Result<u32, ConnectionValueError> {
    if value > MAX_KEEPALIVE_INTERVAL_SECS {
        return Err(ConnectionValueError::KeepaliveIntervalOutOfRange {
            value,
            max: MAX_KEEPALIVE_INTERVAL_SECS,
        });
    }
    Ok(value)
}

fn check_misses(value: u32) -> Result<u32, ConnectionValueError> {
    if value == 0 || value > MAX_KEEPALIVE_MAX_MISSES {
        return Err(ConnectionValueError::KeepaliveMaxMissesOutOfRange {
            value,
            max: MAX_KEEPALIVE_MAX_MISSES,
        });
    }
    Ok(value)
}

fn check_timeout(value: u32) -> Result<u32, ConnectionValueError> {
    if value == 0 || value > MAX_CONNECT_TIMEOUT_SECS {
        return Err(ConnectionValueError::ConnectTimeoutOutOfRange {
            value,
            max: MAX_CONNECT_TIMEOUT_SECS,
        });
    }
    Ok(value)
}

/// What one host says about its own connection behaviour (R20).
///
/// Every field is optional and independent, which is what makes the three cases
/// distinguishable: absent means the global default applies,
/// `keepalive_interval_secs = 0` turns keepalive off for this host, and a value states
/// an override. The fields are separate because OpenSSH's two directives are separate
/// too: a file that sets only `ServerAliveCountMax` keeps the global interval.
///
/// The record is empty when it says nothing, and an empty record is stored as no
/// field at all, so a host saved before the choice existed keeps its shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HostConnectionWire")]
pub struct HostConnection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keepalive_interval_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keepalive_max_misses: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connect_timeout_secs: Option<u32>,
}

/// The stored shape of a host's connection overrides.
#[derive(Deserialize)]
struct HostConnectionWire {
    #[serde(default)]
    keepalive_interval_secs: Option<u32>,
    #[serde(default)]
    keepalive_max_misses: Option<u32>,
    #[serde(default)]
    connect_timeout_secs: Option<u32>,
}

impl TryFrom<HostConnectionWire> for HostConnection {
    type Error = ConnectionValueError;

    fn try_from(wire: HostConnectionWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.keepalive_interval_secs,
            wire.keepalive_max_misses,
            wire.connect_timeout_secs,
        )
    }
}

impl HostConnection {
    /// A record from the values a form or an import produced, each already parsed.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionValueError`] for a probe count or a timeout of `0`, a
    /// count above [`MAX_KEEPALIVE_MAX_MISSES`], an interval above
    /// [`MAX_KEEPALIVE_INTERVAL_SECS`] or a timeout above
    /// [`MAX_CONNECT_TIMEOUT_SECS`].
    pub fn new(
        keepalive_interval_secs: Option<u32>,
        keepalive_max_misses: Option<u32>,
        connect_timeout_secs: Option<u32>,
    ) -> Result<Self, ConnectionValueError> {
        Ok(Self {
            keepalive_interval_secs: keepalive_interval_secs.map(check_interval).transpose()?,
            keepalive_max_misses: keepalive_max_misses.map(check_misses).transpose()?,
            connect_timeout_secs: connect_timeout_secs.map(check_timeout).transpose()?,
        })
    }

    /// The record that turns keepalive off for this host.
    #[must_use]
    pub fn keepalive_off() -> Self {
        Self {
            keepalive_interval_secs: Some(0),
            ..Self::default()
        }
    }

    /// The interval this host chose, in seconds; `Some(0)` means keepalive is off.
    #[must_use]
    pub fn keepalive_interval_secs(&self) -> Option<u32> {
        self.keepalive_interval_secs
    }

    /// The probe count this host chose, when it chose one.
    #[must_use]
    pub fn keepalive_max_misses(&self) -> Option<u32> {
        self.keepalive_max_misses
    }

    /// The connect timeout this host chose, in seconds.
    #[must_use]
    pub fn connect_timeout_secs(&self) -> Option<u32> {
        self.connect_timeout_secs
    }

    /// Whether this record states nothing and the global default applies.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keepalive_interval_secs.is_none()
            && self.keepalive_max_misses.is_none()
            && self.connect_timeout_secs.is_none()
    }

    /// Sets or clears the keepalive interval. `Some(0)` turns keepalive off.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionValueError::KeepaliveIntervalOutOfRange`] above the
    /// supported interval.
    pub fn set_keepalive_interval_secs(
        &mut self,
        seconds: Option<u32>,
    ) -> Result<(), ConnectionValueError> {
        self.keepalive_interval_secs = seconds.map(check_interval).transpose()?;
        Ok(())
    }

    /// Sets or clears the number of unanswered probes.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionValueError::KeepaliveMaxMissesOutOfRange`] for `0` and
    /// for values above the supported count.
    pub fn set_keepalive_max_misses(
        &mut self,
        misses: Option<u32>,
    ) -> Result<(), ConnectionValueError> {
        self.keepalive_max_misses = misses.map(check_misses).transpose()?;
        Ok(())
    }

    /// Sets or clears the connect timeout.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionValueError::ConnectTimeoutOutOfRange`] for `0` and for
    /// values above the supported timeout.
    pub fn set_connect_timeout_secs(
        &mut self,
        seconds: Option<u32>,
    ) -> Result<(), ConnectionValueError> {
        self.connect_timeout_secs = seconds.map(check_timeout).transpose()?;
        Ok(())
    }
}

/// The global connection defaults, saved in the preferences file.
///
/// They are the values every host that says nothing is dialled with, which is why they
/// are bounded and validated like a host's own numbers. A missing field keeps the
/// documented default rather than `0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ConnectionDefaultsWire")]
pub struct ConnectionDefaults {
    keepalive_interval_secs: u32,
    keepalive_max_misses: u32,
    connect_timeout_secs: u32,
}

/// The stored shape of the global defaults; absent fields keep the documented value.
#[derive(Deserialize)]
struct ConnectionDefaultsWire {
    #[serde(default = "default_interval")]
    keepalive_interval_secs: u32,
    #[serde(default = "default_misses")]
    keepalive_max_misses: u32,
    #[serde(default = "default_timeout")]
    connect_timeout_secs: u32,
}

fn default_interval() -> u32 {
    DEFAULT_KEEPALIVE_INTERVAL_SECS
}

fn default_misses() -> u32 {
    DEFAULT_KEEPALIVE_MAX_MISSES
}

fn default_timeout() -> u32 {
    DEFAULT_CONNECT_TIMEOUT_SECS
}

impl Default for ConnectionDefaults {
    fn default() -> Self {
        Self {
            keepalive_interval_secs: DEFAULT_KEEPALIVE_INTERVAL_SECS,
            keepalive_max_misses: DEFAULT_KEEPALIVE_MAX_MISSES,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
        }
    }
}

impl TryFrom<ConnectionDefaultsWire> for ConnectionDefaults {
    type Error = ConnectionValueError;

    fn try_from(wire: ConnectionDefaultsWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.keepalive_interval_secs,
            wire.keepalive_max_misses,
            wire.connect_timeout_secs,
        )
    }
}

impl ConnectionDefaults {
    /// Validated global defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionValueError`] for the same values a host's own record
    /// refuses.
    pub fn new(
        keepalive_interval_secs: u32,
        keepalive_max_misses: u32,
        connect_timeout_secs: u32,
    ) -> Result<Self, ConnectionValueError> {
        Ok(Self {
            keepalive_interval_secs: check_interval(keepalive_interval_secs)?,
            keepalive_max_misses: check_misses(keepalive_max_misses)?,
            connect_timeout_secs: check_timeout(connect_timeout_secs)?,
        })
    }

    /// The default keepalive interval, in seconds; `0` means every host without its
    /// own value has keepalive off.
    #[must_use]
    pub fn keepalive_interval_secs(&self) -> u32 {
        self.keepalive_interval_secs
    }

    /// The default number of unanswered probes.
    #[must_use]
    pub fn keepalive_max_misses(&self) -> u32 {
        self.keepalive_max_misses
    }

    /// The default connect timeout, in seconds.
    #[must_use]
    pub fn connect_timeout_secs(&self) -> u32 {
        self.connect_timeout_secs
    }

    /// The policy these defaults alone produce.
    #[must_use]
    pub fn policy(&self) -> ConnectionPolicy {
        self.policy_for(None)
    }

    /// The policy one host is dialled with: its own values where it states them, the
    /// global default everywhere else.
    ///
    /// A host interval of `0` turns keepalive off for that host only; a host that
    /// states just a probe count keeps the global interval, which is how OpenSSH's two
    /// independent directives behave.
    #[must_use]
    pub fn policy_for(&self, host: Option<HostConnection>) -> ConnectionPolicy {
        let host = host.unwrap_or_default();
        let interval = host
            .keepalive_interval_secs()
            .unwrap_or(self.keepalive_interval_secs);
        let keepalive = (interval > 0).then(|| Keepalive {
            interval: Duration::from_secs(u64::from(interval)),
            max_misses: host
                .keepalive_max_misses()
                .unwrap_or(self.keepalive_max_misses),
        });
        ConnectionPolicy {
            keepalive,
            connect_timeout: Duration::from_secs(u64::from(
                host.connect_timeout_secs()
                    .unwrap_or(self.connect_timeout_secs),
            )),
        }
    }
}

/// Keepalive as it is actually applied: how long to wait, and how many silent probes
/// mean the connection is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keepalive {
    interval: Duration,
    max_misses: u32,
}

impl Keepalive {
    /// How long the connection may be silent before a probe is sent.
    #[must_use]
    pub fn interval(self) -> Duration {
        self.interval
    }

    /// How many probes may go unanswered before the connection is treated as dead.
    #[must_use]
    pub fn max_misses(self) -> u32 {
        self.max_misses
    }

    /// The sentence a settings page or a preview can show (R20).
    #[must_use]
    pub fn summary(self) -> String {
        format!(
            "keepalive every {}s, dropped after {} unanswered",
            self.interval.as_secs(),
            self.max_misses
        )
    }
}

/// What a connection attempt is really dialled with (R20).
///
/// It is the same value for a direct session and for every hop of a jump chain, so a
/// chain cannot end up with one hop probing and another not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionPolicy {
    keepalive: Option<Keepalive>,
    connect_timeout: Duration,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        ConnectionDefaults::default().policy()
    }
}

impl ConnectionPolicy {
    /// The resolved keepalive, or `None` when it is switched off.
    ///
    /// `None` is not "use a library default": the caller writes no interval at all, so
    /// no probe is ever sent and nothing may claim the link is being watched.
    #[must_use]
    pub fn keepalive(&self) -> Option<Keepalive> {
        self.keepalive
    }

    /// The interval to probe at, or `None` when keepalive is off.
    #[must_use]
    pub fn keepalive_interval(&self) -> Option<Duration> {
        self.keepalive.map(Keepalive::interval)
    }

    /// How many unanswered probes end the connection.
    ///
    /// A disabled keepalive answers with the default count: the number is never used,
    /// because no probe is sent, and a caller that writes it into the transport config
    /// must also see `None` from [`Self::keepalive_interval`].
    #[must_use]
    pub fn keepalive_max(&self) -> usize {
        let misses = self
            .keepalive
            .map_or(DEFAULT_KEEPALIVE_MAX_MISSES, Keepalive::max_misses);
        usize::try_from(misses).unwrap_or(usize::MAX)
    }

    /// The deadline for the machine part of an attempt.
    ///
    /// It covers dialling and the key exchange up to the identity question, and it
    /// stops there: comparing a fingerprint is the user's work, not a slow server.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// The same policy with another deadline, for a caller that already resolved one.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// The inactivity timeout a session is dialled with.
    ///
    /// Always `None`. The library's inactivity timeout closes a session that has been
    /// quiet, which a working idle shell is; keepalive is the probe that tells a quiet
    /// link apart from a dead one, and it is the only mechanism this build uses (R20).
    #[must_use]
    pub fn inactivity_timeout(&self) -> Option<Duration> {
        None
    }

    /// One line for a settings summary or an import preview.
    #[must_use]
    pub fn summary(&self) -> String {
        let keepalive = match self.keepalive {
            Some(keepalive) => keepalive.summary(),
            None => "keepalive off".to_owned(),
        };
        format!(
            "{keepalive}; connect timeout {}s",
            self.connect_timeout.as_secs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_documented_thirty_three_and_twenty() {
        let defaults = ConnectionDefaults::default();
        assert_eq!(defaults.keepalive_interval_secs(), 30);
        assert_eq!(defaults.keepalive_max_misses(), 3);
        assert_eq!(defaults.connect_timeout_secs(), 20);
        let policy = defaults.policy();
        assert_eq!(policy.keepalive_interval(), Some(Duration::from_secs(30)));
        assert_eq!(policy.keepalive_max(), 3);
        assert_eq!(policy.connect_timeout(), Duration::from_secs(20));
        assert_eq!(policy, ConnectionPolicy::default());
    }

    #[test]
    fn a_host_that_says_nothing_inherits_the_global_defaults() {
        let defaults = ConnectionDefaults::new(45, 5, 10).expect("valid defaults");
        let inherited = defaults.policy_for(None);
        assert_eq!(
            inherited.keepalive_interval(),
            Some(Duration::from_secs(45))
        );
        assert_eq!(inherited.keepalive_max(), 5);
        assert_eq!(inherited.connect_timeout(), Duration::from_secs(10));

        let empty = defaults.policy_for(Some(HostConnection::default()));
        assert_eq!(
            empty, inherited,
            "an empty record is not an override; it is the absence of one"
        );
    }

    #[test]
    fn a_host_can_turn_keepalive_off_without_touching_the_default() {
        let defaults = ConnectionDefaults::default();
        let off = defaults.policy_for(Some(HostConnection::keepalive_off()));
        assert!(off.keepalive().is_none(), "an explicit off applies");
        assert_eq!(off.keepalive_interval(), None);
        assert_eq!(
            off.connect_timeout(),
            Duration::from_secs(20),
            "the untouched field still inherits"
        );
        assert_eq!(
            defaults.policy().keepalive_interval(),
            Some(Duration::from_secs(30)),
            "another host keeps the default"
        );

        let disabled = ConnectionDefaults::new(0, 3, 20).expect("0 is a valid interval");
        assert!(disabled.policy().keepalive().is_none());
        assert!(disabled.policy_for(None).keepalive().is_none());
    }

    #[test]
    fn a_host_overrides_only_what_it_states() {
        let defaults = ConnectionDefaults::default();
        let keepalive_only = HostConnection::new(Some(120), Some(7), None).expect("valid");
        let policy = defaults.policy_for(Some(keepalive_only));
        assert_eq!(policy.keepalive_interval(), Some(Duration::from_secs(120)));
        assert_eq!(policy.keepalive_max(), 7);
        assert_eq!(policy.connect_timeout(), Duration::from_secs(20));

        let timeout_only = HostConnection::new(None, None, Some(5)).expect("valid");
        let policy = defaults.policy_for(Some(timeout_only));
        assert_eq!(policy.keepalive_interval(), Some(Duration::from_secs(30)));
        assert_eq!(policy.keepalive_max(), 3);
        assert_eq!(policy.connect_timeout(), Duration::from_secs(5));

        let count_only = HostConnection::new(None, Some(9), None).expect("valid");
        let policy = defaults.policy_for(Some(count_only));
        assert_eq!(policy.keepalive_interval(), Some(Duration::from_secs(30)));
        assert_eq!(policy.keepalive_max(), 9);
    }

    #[test]
    fn changing_the_default_does_not_change_a_host_that_overrode_it() {
        let host = HostConnection::new(Some(15), Some(2), Some(5)).expect("valid");
        let before = ConnectionDefaults::new(30, 3, 20)
            .expect("valid")
            .policy_for(Some(host));
        let after = ConnectionDefaults::new(90, 6, 45)
            .expect("valid")
            .policy_for(Some(host));
        assert_eq!(before, after, "an explicit override is not a default");
        assert_eq!(after.keepalive_interval(), Some(Duration::from_secs(15)));
        assert_eq!(after.keepalive_max(), 2);
        assert_eq!(after.connect_timeout(), Duration::from_secs(5));

        // A field the host did not state keeps following the default, so one changed
        // default moves only the values nobody overrode.
        let partial = HostConnection::new(Some(15), None, None).expect("valid");
        let before = ConnectionDefaults::new(30, 3, 20)
            .expect("valid")
            .policy_for(Some(partial));
        let after = ConnectionDefaults::new(90, 6, 45)
            .expect("valid")
            .policy_for(Some(partial));
        assert_eq!(before.keepalive_interval(), Some(Duration::from_secs(15)));
        assert_eq!(after.keepalive_max(), 6);
        assert_eq!(after.connect_timeout(), Duration::from_secs(45));
    }

    #[test]
    fn a_healthy_idle_session_is_never_timed_out_for_being_quiet() {
        let policies = [
            ConnectionPolicy::default(),
            ConnectionDefaults::new(0, 3, 20).expect("valid").policy(),
            ConnectionDefaults::new(600, 1, 5).expect("valid").policy(),
        ];
        for policy in policies {
            assert_eq!(
                policy.inactivity_timeout(),
                None,
                "an idle but answered session is not closed for silence"
            );
        }
    }

    #[test]
    fn an_unusable_number_is_refused_with_its_reason() {
        assert_eq!(
            ConnectionDefaults::new(MAX_KEEPALIVE_INTERVAL_SECS + 1, 3, 20),
            Err(ConnectionValueError::KeepaliveIntervalOutOfRange {
                value: MAX_KEEPALIVE_INTERVAL_SECS + 1,
                max: MAX_KEEPALIVE_INTERVAL_SECS,
            })
        );
        assert_eq!(
            ConnectionDefaults::new(30, 0, 20),
            Err(ConnectionValueError::KeepaliveMaxMissesOutOfRange {
                value: 0,
                max: MAX_KEEPALIVE_MAX_MISSES,
            })
        );
        assert_eq!(
            ConnectionDefaults::new(30, 3, 0),
            Err(ConnectionValueError::ConnectTimeoutOutOfRange {
                value: 0,
                max: MAX_CONNECT_TIMEOUT_SECS,
            })
        );
        assert_eq!(
            ConnectionDefaults::new(30, 3, MAX_CONNECT_TIMEOUT_SECS + 1),
            Err(ConnectionValueError::ConnectTimeoutOutOfRange {
                value: MAX_CONNECT_TIMEOUT_SECS + 1,
                max: MAX_CONNECT_TIMEOUT_SECS,
            })
        );
        assert_eq!(
            HostConnection::new(Some(30), Some(0), None),
            Err(ConnectionValueError::KeepaliveMaxMissesOutOfRange {
                value: 0,
                max: MAX_KEEPALIVE_MAX_MISSES,
            })
        );
        let message = ConnectionValueError::KeepaliveMaxMissesOutOfRange {
            value: 0,
            max: MAX_KEEPALIVE_MAX_MISSES,
        }
        .to_string();
        assert!(message.contains('0'), "{message}");
        assert!(message.contains("silent connection"), "{message}");
    }

    #[test]
    fn a_stored_value_is_checked_when_it_is_read() {
        let ok: ConnectionDefaults =
            serde_json::from_str(r#"{"keepalive_interval_secs":60}"#).expect("reads");
        assert_eq!(ok.keepalive_interval_secs(), 60);
        assert_eq!(
            ok.keepalive_max_misses(),
            3,
            "a missing field keeps the documented default"
        );
        assert_eq!(ok.connect_timeout_secs(), 20);

        let empty: ConnectionDefaults = serde_json::from_str("{}").expect("reads");
        assert_eq!(empty, ConnectionDefaults::default());

        assert!(
            serde_json::from_str::<ConnectionDefaults>(r#"{"keepalive_max_misses":0}"#).is_err(),
            "a stored count of 0 is refused instead of being applied"
        );
        assert!(
            serde_json::from_str::<HostConnection>(r#"{"connect_timeout_secs":0}"#).is_err(),
            "a stored timeout of 0 is refused rather than read as no deadline"
        );
        assert!(
            serde_json::from_str::<ConnectionDefaults>(r#"{"keepalive_interval_secs":-1}"#)
                .is_err(),
            "a negative interval is not a number this build reads"
        );
    }

    #[test]
    fn a_host_record_is_absent_when_it_states_nothing_and_round_trips_otherwise() {
        let empty = serde_json::to_string(&HostConnection::default()).expect("serializes");
        assert_eq!(empty, "{}", "nothing stated is nothing written");

        let record = HostConnection::new(Some(45), Some(4), Some(8)).expect("valid");
        let json = serde_json::to_string(&record).expect("serializes");
        assert!(json.contains("\"keepalive_interval_secs\":45"), "{json}");
        let back: HostConnection = serde_json::from_str(&json).expect("reads");
        assert_eq!(back, record);

        let off = HostConnection::keepalive_off();
        let json = serde_json::to_string(&off).expect("serializes");
        assert!(json.contains("\"keepalive_interval_secs\":0"), "{json}");
        let back: HostConnection = serde_json::from_str(&json).expect("reads");
        assert_eq!(back.keepalive_interval_secs(), Some(0));
        assert!(
            ConnectionDefaults::default()
                .policy_for(Some(back))
                .keepalive()
                .is_none(),
            "0 is an explicit off that survives a save"
        );
    }

    #[test]
    fn the_two_forms_of_writing_a_number_agree() {
        let from_new = HostConnection::new(Some(60), Some(5), Some(15)).expect("valid");
        let mut built = HostConnection::default();
        built.set_keepalive_interval_secs(Some(60)).expect("valid");
        built.set_keepalive_max_misses(Some(5)).expect("valid");
        built.set_connect_timeout_secs(Some(15)).expect("valid");
        assert_eq!(from_new, built);
        assert!(!built.is_empty());

        let mut cleared = built;
        cleared.set_keepalive_interval_secs(None).expect("valid");
        cleared.set_keepalive_max_misses(None).expect("valid");
        cleared.set_connect_timeout_secs(None).expect("valid");
        assert!(cleared.is_empty(), "clearing every field is inheritance");
    }

    #[test]
    fn the_summary_says_what_will_actually_happen() {
        let enabled = ConnectionDefaults::default().policy();
        assert!(
            enabled.summary().contains("every 30s"),
            "{}",
            enabled.summary()
        );
        assert!(
            enabled.summary().contains("3 unanswered"),
            "{}",
            enabled.summary()
        );
        assert!(enabled.summary().contains("connect timeout 20s"));

        let off = ConnectionDefaults::new(0, 3, 20).expect("valid").policy();
        assert!(off.summary().contains("keepalive off"), "{}", off.summary());
        assert!(
            !off.summary().contains("every"),
            "a disabled keepalive is not described as probing: {}",
            off.summary()
        );
    }
}
