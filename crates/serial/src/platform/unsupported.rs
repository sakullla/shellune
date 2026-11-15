//! The backend for a system this crate has no serial API for.
//!
//! It exists so the crate still builds - and so a caller on such a system is told the
//! truth instead of being handed a session that would never carry a byte.

use std::sync::Arc;

use crate::config::SerialConfig;
use crate::error::{SerialCause, SerialError};
use crate::io::PortIo;
use crate::port::PortInfo;

pub(crate) fn open(_port: &str, _config: &SerialConfig) -> Result<Arc<dyn PortIo>, SerialError> {
    Err(SerialError::new(SerialCause::NotFound)
        .with_detail("this system has no serial port backend in this build"))
}

pub(crate) fn list() -> Result<Vec<PortInfo>, SerialError> {
    Err(SerialError::new(SerialCause::Enumerate)
        .with_detail("this system has no serial port enumeration in this build"))
}
