//! Why a record could not be changed, or a file could not be read or written.
//!
//! Every variant is structured and named after the layer that failed, so the shell
//! can show a reason it actually received. No variant carries host credentials:
//! the model has none to carry.

use std::path::PathBuf;

use shellune_core::HostId;

/// Why a change to the saved records was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BookError {
    /// A host with this identifier is already saved.
    #[error("a host with id {id} is already saved")]
    DuplicateHost {
        /// The identifier that collided.
        id: HostId,
    },
    /// The host to change does not exist.
    #[error("no saved host with id {id} exists")]
    UnknownHost {
        /// The identifier that was not found.
        id: HostId,
    },
    /// The host file holds two records for the same identifier.
    #[error("the saved hosts are inconsistent: {reason}")]
    Inconsistent {
        /// What is wrong with the stored records.
        reason: String,
    },
}

/// Why the host file could not be loaded or saved.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    /// The file exists but could not be read.
    #[error("the saved hosts at {path} could not be read: {reason}")]
    Read {
        /// The file that was read.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The file was read but does not hold the document this build writes.
    #[error("the saved hosts at {path} are not valid: {reason}")]
    Corrupt {
        /// The file that is damaged.
        path: PathBuf,
        /// What could not be understood.
        reason: String,
    },
    /// The file was written by a newer build.
    #[error("the saved hosts at {path} use format {found}, but this build reads up to {supported}")]
    UnsupportedVersion {
        /// The file that was read.
        path: PathBuf,
        /// The format version found in the file.
        found: u32,
        /// The newest format this build understands.
        supported: u32,
    },
    /// The directory the file belongs in could not be created.
    #[error("the hosts directory {path} could not be created: {reason}")]
    CreateDirectory {
        /// The directory that was attempted.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The temporary file could not be written.
    #[error("the saved hosts at {path} could not be written: {reason}")]
    Write {
        /// The file that was being written.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The temporary file could not be moved over the target.
    #[error("the saved hosts at {path} could not be replaced: {reason}")]
    Replace {
        /// The file that could not be replaced.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The records could not be encoded at all.
    #[error("the host records could not be encoded: {reason}")]
    Encode {
        /// What the serializer reported.
        reason: String,
    },
}

impl StorageError {
    /// The file the failure is about.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Read { path, .. }
            | Self::Corrupt { path, .. }
            | Self::UnsupportedVersion { path, .. }
            | Self::CreateDirectory { path, .. }
            | Self::Write { path, .. }
            | Self::Replace { path, .. } => Some(path),
            Self::Encode { .. } => None,
        }
    }
}

/// Why a workspace snapshot or a forwarding file could not be read or written.
///
/// `label` is the document ("workspace snapshot", "forwarding rules"), so the
/// message names the file and the next step. A secret value is not part of any
/// variant: reasons describe the structure, not the contents of a credential.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PersistError {
    /// The file exists but could not be read.
    #[error("the {label} at {path} could not be read: {reason}")]
    Read {
        /// Which document was being read.
        label: &'static str,
        /// The file that was read.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The file is not a document this build understands.
    #[error("the {label} at {path} is not valid: {reason}")]
    Corrupt {
        /// Which document was being read.
        label: &'static str,
        /// The file that is damaged.
        path: PathBuf,
        /// What could not be understood, without a stored secret.
        reason: String,
    },
    /// The file was written by a newer build and was left as it was.
    #[error(
        "the {label} at {path} uses format {found}, but this build reads up to {supported}; the file was left unchanged"
    )]
    UnsupportedVersion {
        /// Which document was being read.
        label: &'static str,
        /// The file that was read.
        path: PathBuf,
        /// The format version found in the file.
        found: u32,
        /// The newest format this build understands.
        supported: u32,
    },
    /// The directory the file belongs in could not be created.
    #[error("the {label} directory {path} could not be created: {reason}")]
    CreateDirectory {
        /// Which document was being written.
        label: &'static str,
        /// The directory that was attempted.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The temporary file could not be written.
    #[error("the {label} at {path} could not be written: {reason}")]
    Write {
        /// Which document was being written.
        label: &'static str,
        /// The file that was being written.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The temporary file could not be moved over the target.
    #[error("the {label} at {path} could not be replaced: {reason}")]
    Replace {
        /// Which document was being written.
        label: &'static str,
        /// The file that could not be replaced.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The records could not be encoded at all.
    #[error("the {label} could not be encoded: {reason}")]
    Encode {
        /// Which document was being written.
        label: &'static str,
        /// What the serializer reported.
        reason: String,
    },
}

impl PersistError {
    /// The file the failure is about.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Read { path, .. }
            | Self::Corrupt { path, .. }
            | Self::UnsupportedVersion { path, .. }
            | Self::CreateDirectory { path, .. }
            | Self::Write { path, .. }
            | Self::Replace { path, .. } => Some(path),
            Self::Encode { .. } => None,
        }
    }
}
