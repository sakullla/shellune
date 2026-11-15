//! The per-platform backend: opening a device, listing the ports, stopping I/O (R21).
//!
//! This module is the only place in the crate that touches a raw handle or a file
//! descriptor. Everything above it - [`crate::SerialSession`], the queue, the events -
//! works through [`crate::io::PortIo`] and knows nothing about the platform, which is
//! what keeps the unsafe code in two files and the tests independent of a device.

use std::sync::Arc;

use crate::config::SerialConfig;
use crate::error::SerialError;
use crate::io::PortIo;
use crate::port::PortInfo;

#[cfg(unix)]
mod unix;
#[cfg(not(any(windows, unix)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as backend;
#[cfg(not(any(windows, unix)))]
use unsupported as backend;
#[cfg(windows)]
use windows as backend;

/// Opens a device with the given settings.
pub(crate) fn open(port: &str, config: &SerialConfig) -> Result<Arc<dyn PortIo>, SerialError> {
    backend::open(port, config)
}

/// Lists the ports this machine reports.
pub(crate) fn list() -> Result<Vec<PortInfo>, SerialError> {
    backend::list()
}
