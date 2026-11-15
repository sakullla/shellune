//! One place where a system code becomes a cause the user can act on (R17, R21).
//!
//! The two backends do one thing only: translate the code the system returned into a
//! [`SysCode`]. The decision about what that code means - which codes separate "in
//! use" from "permission denied", which ones the system cannot separate and must be
//! kept as a combination, which mean "the device went away" - lives here, so the same
//! table is unit tested on any host and the two platforms cannot drift apart.
//!
//! What the tables encode is what the systems actually document:
//!
//! * opening a Windows COM port that another program holds fails with
//!   `ERROR_ACCESS_DENIED` (5) - the same code a permission problem returns, which is
//!   why that code keeps both causes; `ERROR_SHARING_VIOLATION` (32) and Unix
//!   `EBUSY` from an exclusive claim are unambiguous "in use";
//! * `ERROR_GEN_FAILURE` (31) and Unix `EIO` are what a driver reports for both "this
//!   setting is impossible" and "this device just failed", so they keep both causes
//!   instead of guessing;
//! * `ERROR_DEVICE_NOT_CONNECTED` (1167), `ERROR_DEV_NOT_EXIST` (55) and Unix
//!   `ENXIO`/`ENODEV` are a device that is gone.

use crate::error::SerialCause;

/// The code a backend reported, already translated to a meaning both platforms share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SysCode {
    /// No device of that name.
    NoSuchDevice,
    /// The caller may not open it.
    AccessDenied,
    /// Something else holds it.
    Busy,
    /// The setting or the name is not acceptable.
    InvalidParameter,
    /// The device is gone.
    DeviceGone,
    /// The driver failed without saying why.
    NotFunctioning,
    /// The operation was cancelled.
    Aborted,
    /// A code this crate has no meaning for.
    Other,
}

/// What the port was being asked to do, which changes what a code means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Opening the device.
    Open,
    /// Applying the line settings to the opened device.
    Configure,
    /// Reading or writing the opened device.
    Io,
}

/// What a code means at this stage: the primary cause and any it cannot be split from.
pub(crate) fn classify(stage: Stage, code: SysCode) -> (SerialCause, &'static [SerialCause]) {
    match (stage, code) {
        (_, SysCode::Aborted) => (SerialCause::Io, &[]),
        (Stage::Open, SysCode::NoSuchDevice) => (SerialCause::NotFound, &[]),
        (Stage::Open, SysCode::AccessDenied) => {
            (SerialCause::PermissionDenied, &[SerialCause::InUse])
        }
        (Stage::Open, SysCode::Busy) => (SerialCause::InUse, &[]),
        (Stage::Open, SysCode::InvalidParameter) => {
            (SerialCause::NotFound, &[SerialCause::UnsupportedConfig])
        }
        (Stage::Open, SysCode::DeviceGone) => (SerialCause::Disconnected, &[]),
        (Stage::Open, SysCode::NotFunctioning) => (SerialCause::NotFound, &[SerialCause::Io]),
        (Stage::Configure, SysCode::InvalidParameter) => (SerialCause::UnsupportedConfig, &[]),
        (Stage::Configure, SysCode::NotFunctioning) => {
            (SerialCause::UnsupportedConfig, &[SerialCause::Io])
        }
        (Stage::Configure, SysCode::AccessDenied) => (SerialCause::PermissionDenied, &[]),
        (Stage::Configure, SysCode::DeviceGone) => (SerialCause::Disconnected, &[]),
        (Stage::Configure, SysCode::Busy) => (SerialCause::InUse, &[]),
        (Stage::Configure, SysCode::NoSuchDevice) => (SerialCause::NotFound, &[]),
        (Stage::Io, SysCode::DeviceGone) => (SerialCause::Disconnected, &[]),
        (Stage::Io, SysCode::NotFunctioning) => (SerialCause::Disconnected, &[SerialCause::Io]),
        (Stage::Io, SysCode::AccessDenied) => (SerialCause::PermissionDenied, &[]),
        (Stage::Io, SysCode::Busy) => (SerialCause::Io, &[]),
        (Stage::Io, SysCode::NoSuchDevice) => (SerialCause::Disconnected, &[]),
        (Stage::Io, SysCode::InvalidParameter) => (SerialCause::Io, &[]),
        (_, SysCode::Other) => (SerialCause::Io, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary(stage: Stage, code: SysCode) -> SerialCause {
        classify(stage, code).0
    }

    #[test]
    fn a_port_that_is_there_and_free_is_not_reported_as_anything_else() {
        // The code that only means "not there" is the only one reported as missing.
        assert_eq!(
            primary(Stage::Open, SysCode::NoSuchDevice),
            SerialCause::NotFound
        );
    }

    #[test]
    fn windows_access_denied_keeps_both_causes_it_cannot_separate() {
        let (cause, also) = classify(Stage::Open, SysCode::AccessDenied);
        assert_eq!(cause, SerialCause::PermissionDenied);
        assert_eq!(also, [SerialCause::InUse]);
    }

    #[test]
    fn a_sharing_violation_or_unix_busy_is_unambiguously_in_use() {
        assert_eq!(primary(Stage::Open, SysCode::Busy), SerialCause::InUse);
        assert_eq!(classify(Stage::Open, SysCode::Busy).1, []);
    }

    #[test]
    fn a_driver_failure_while_configuring_keeps_unsupported_and_io() {
        let (cause, also) = classify(Stage::Configure, SysCode::NotFunctioning);
        assert_eq!(cause, SerialCause::UnsupportedConfig);
        assert_eq!(also, [SerialCause::Io]);
    }

    #[test]
    fn an_unsupported_setting_and_a_failed_device_stay_apart_where_the_system_does() {
        assert_eq!(
            classify(Stage::Configure, SysCode::InvalidParameter),
            (SerialCause::UnsupportedConfig, &[][..])
        );
        assert_eq!(
            classify(Stage::Io, SysCode::DeviceGone),
            (SerialCause::Disconnected, &[][..])
        );
    }

    #[test]
    fn a_device_that_vanishes_mid_session_is_a_disconnect() {
        assert_eq!(
            primary(Stage::Io, SysCode::DeviceGone),
            SerialCause::Disconnected
        );
        let (cause, also) = classify(Stage::Io, SysCode::NotFunctioning);
        assert_eq!(cause, SerialCause::Disconnected);
        assert_eq!(also, [SerialCause::Io]);
    }

    #[test]
    fn an_unknown_code_is_an_io_failure_not_a_guess() {
        assert_eq!(
            classify(Stage::Io, SysCode::Other),
            (SerialCause::Io, &[][..])
        );
    }
}
