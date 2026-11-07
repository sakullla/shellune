//! The file the forwarding rules live in.
//!
//! Loading is strict and saving is atomic, same as the host file. A missing file
//! is an empty book. A newer file is left untouched. Restoring the book does not
//! bind a port: the rules are the saved configuration and nothing else.

use std::path::{Path, PathBuf};

use shellune_core::{FORWARD_FORMAT_VERSION, ForwardBook};

use crate::error::PersistError;
use crate::store::{load_versioned, save_versioned};

/// Name of the forwarding file inside the data directory.
pub const FILE_NAME: &str = "forwards.json";

const LABEL: &str = "forwarding rules";

/// A forwarding file at a known location.
#[derive(Clone, Debug)]
pub struct ForwardStore {
    root: PathBuf,
}

impl ForwardStore {
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

    /// The directory the rules live in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The rule file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// Reads the rules.
    ///
    /// A missing file is a first run and yields an empty book. No listener is opened.
    ///
    /// # Errors
    ///
    /// Returns the read failure, the corruption, or the unsupported-version failure.
    /// In every case the file that was there is left as it was.
    pub fn load(&self) -> Result<ForwardBook, PersistError> {
        load_versioned(
            &self.root,
            FILE_NAME,
            FORWARD_FORMAT_VERSION,
            LABEL,
            ForwardBook::validate,
        )
    }

    /// Writes the rules atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write, replace or encode failure. The previous file
    /// stays in place, and nothing is bound.
    pub fn save(&self, book: &ForwardBook) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(book).map_err(|error| PersistError::Encode {
            label: LABEL,
            reason: error.to_string(),
        })?;
        save_versioned(&self.root, FILE_NAME, LABEL, &bytes)
    }
}
