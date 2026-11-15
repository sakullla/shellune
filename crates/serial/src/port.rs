//! The ports this machine has, and the one call that opens one (R21).
//!
//! R21 asks that the ports be discoverable and refreshable, and that a missing port be
//! reported as missing rather than as an empty session. This module is the list and the
//! open call; the per-platform work is in the crate-private backend module.

use std::sync::Arc;

use crate::error::SerialError;
use crate::io::PortIo;
use crate::platform;

/// One serial port this machine reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    name: String,
    path: String,
    description: Option<String>,
}

impl PortInfo {
    /// Builds a port entry. A backend uses this; the application only reads it.
    #[must_use]
    pub(crate) fn new(name: String, path: String, description: Option<String>) -> Self {
        Self {
            name,
            path,
            description,
        }
    }

    /// A listed port built by hand, for a test or a capture fixture.
    ///
    /// Nothing in the application builds a port: the list comes from the system, and a
    /// hand-made entry could claim a device that is not there. It exists behind
    /// `test-support` so a test can drive what a list *containing* a port does on a
    /// machine whose list is empty - which is the situation of this repository's
    /// development machine.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn sample(name: &str, path: &str, description: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            description: description.map(str::to_string),
        }
    }

    /// The short name the user sees, such as `COM3` or `ttyUSB0`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The path the port is opened by, such as `\\.\COM3` or `/dev/ttyUSB0`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the system says the device is, when the system says anything.
    ///
    /// The Windows device map only carries port names, so this is `None` there; Unix
    /// reports the driver name when `/sys` describes the device.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// Lists the serial ports this machine reports right now.
///
/// # Errors
///
/// [`crate::SerialCause::Enumerate`] when the list itself could not be read. An
/// empty list is not an error: a machine without serial ports has none.
pub fn list_ports() -> Result<Vec<PortInfo>, SerialError> {
    platform::list()
}

/// Opens a port with the given settings and starts its session.
///
/// The settings are validated before the device is touched, so an impossible
/// combination is refused with [`crate::SerialCause::UnsupportedConfig`] without
/// opening anything.
///
/// # Errors
///
/// The cause the system allowed: a missing port, a port another program holds, a
/// missing permission, settings the driver rejects, a device that is gone, or an
/// input/output failure. The raw code and message are kept with it.
pub(crate) fn open_port(
    port: &str,
    config: &crate::SerialConfig,
) -> Result<Arc<dyn PortIo>, SerialError> {
    config.validate()?;
    platform::open(port, config)
}
