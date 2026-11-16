//! Why an update step did not happen.

/// Why a check, download or install did not succeed.
///
/// Every variant leaves the running copy as it was: a download is only kept once
/// it verifies, and an install that fails puts the old files back first.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// The updater was given a setting it cannot use (an address, a public key, a
    /// version).
    #[error("the update settings are not usable: {reason}")]
    InvalidConfig {
        /// Why.
        reason: String,
    },
    /// The server did not answer in time.
    #[error("the update server did not answer in time")]
    Timeout,
    /// The server could not be reached: no network, a wrong name, a refused or
    /// broken connection, an untrusted certificate or an unusable proxy.
    #[error("the update server could not be reached: {reason}")]
    Unreachable {
        /// What failed.
        reason: String,
    },
    /// The server answered with an error status.
    #[error("the update server answered HTTP {status}")]
    HttpStatus {
        /// The status.
        status: u16,
    },
    /// The manifest could not be read.
    #[error("the update information could not be read: {reason}")]
    InvalidManifest {
        /// Why.
        reason: String,
    },
    /// The release has no file this copy can install itself.
    #[error("this release has no update file for this installation")]
    NoInstallableAsset,
    /// The release lists the file without a signature.
    #[error("the update file is not signed")]
    SignatureMissing,
    /// The signature does not match the file or the built-in key, or cannot be
    /// read.
    #[error("the update file's signature is not valid: {reason}")]
    SignatureInvalid {
        /// Why.
        reason: String,
    },
    /// The signature is valid but was made for another version than the one the
    /// manifest announces.
    #[error("the update file was signed for {signed} instead of version {expected}")]
    VersionMismatch {
        /// The manifest's version.
        expected: String,
        /// What the signature names (`no version` when it names none).
        signed: String,
    },
    /// The download stopped before the whole file arrived.
    #[error("the download was interrupted: {reason}")]
    DownloadInterrupted {
        /// What happened.
        reason: String,
    },
    /// A file could not be read or written.
    #[error("{context}: {reason}")]
    Io {
        /// What was being done.
        context: String,
        /// The system's reason.
        reason: String,
    },
    /// The update file could not be put in place; whatever was changed has been
    /// put back.
    #[error("the update could not be installed: {reason}")]
    Install {
        /// Why.
        reason: String,
    },
}

impl UpdateError {
    pub(crate) fn io(context: impl Into<String>, error: &std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            reason: error.to_string(),
        }
    }

    pub(crate) fn install(reason: impl Into<String>) -> Self {
        Self::Install {
            reason: reason.into(),
        }
    }
}
