//! The file a workspace snapshot lives in.
//!
//! The store reads and writes the snapshot. It does not restore it into
//! processes: [`WorkspaceSnapshot::restore`](shellune_core::WorkspaceSnapshot::restore)
//! is a pure function, and this module never calls it while loading. A newer or
//! damaged file is reported and left unchanged, rather than replaced with an
//! empty workspace.

use std::path::{Path, PathBuf};

use shellune_core::{WORKSPACE_FORMAT_VERSION, WorkspaceSnapshot};

use crate::error::PersistError;
use crate::store::{load_versioned, save_versioned};

/// Name of the workspace file inside the data directory.
pub const FILE_NAME: &str = "workspace.json";

const LABEL: &str = "workspace snapshot";

/// A workspace file at a known location.
#[derive(Clone, Debug)]
pub struct WorkspaceStore {
    root: PathBuf,
}

impl WorkspaceStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A store beside the host records in the per-user data directory.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(crate::store::data_directory())
    }

    /// The directory the snapshot lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The snapshot file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// Reads the snapshot.
    ///
    /// A missing file is a first run and yields an empty snapshot. Loading does
    /// not restore panes, so it cannot start a shell, a connection or a listener.
    ///
    /// The format version stays at [`WORKSPACE_FORMAT_VERSION`] while only optional
    /// tab fields are added: a tab's own name and its distinguishing colour are
    /// written when they exist and default to "none" when they do not, so a file from
    /// an older build keeps loading instead of being refused as a newer version.
    ///
    /// # Errors
    ///
    /// Returns the read failure, the corruption, or the unsupported-version failure.
    /// The previous file is left as it was.
    pub fn load(&self) -> Result<WorkspaceSnapshot, PersistError> {
        load_versioned(
            &self.root,
            FILE_NAME,
            WORKSPACE_FORMAT_VERSION,
            LABEL,
            |snapshot: &WorkspaceSnapshot| snapshot.validate(),
        )
    }

    /// Writes the snapshot atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write, replace or encode failure. The previous file
    /// stays in place.
    pub fn save(&self, snapshot: &WorkspaceSnapshot) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(snapshot).map_err(|error| PersistError::Encode {
            label: LABEL,
            reason: error.to_string(),
        })?;
        save_versioned(&self.root, FILE_NAME, LABEL, &bytes)
    }
}
