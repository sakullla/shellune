//! The file the host records live in: strict loading, atomic saving.
//!
//! Saving writes a temporary file next to the target, flushes it, and only then
//! renames it over the target. A crash, a full disk or a permission error therefore
//! leaves the previous file exactly as it was: the store never reports a save that
//! did not finish, and it never leaves a half-written host file behind.

use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::audit::StoreEvent;
use crate::book::{FORMAT_VERSION, HostBook};
use crate::error::StorageError;

/// Name of the file inside the data directory.
pub const FILE_NAME: &str = "hosts.json";

/// Where a store sends its log lines.
type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// A host file at a known location.
#[derive(Clone)]
pub struct HostStore {
    root: PathBuf,
    sink: Option<LogSink>,
}

impl fmt::Debug for HostStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl HostStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sink: None,
        }
    }

    /// A store at the per-user data directory of this platform.
    ///
    /// The directory is not created here: a build that never saves a host must not
    /// leave files behind, and the first save creates it.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(data_directory())
    }

    /// The same store, with every event also handed to `sink`.
    ///
    /// This is the one place a logger attaches: the store itself does not know how
    /// the lines are written, so the application can print them, keep them in a
    /// ring buffer or drop them, and the tests can capture them.
    #[must_use]
    pub fn logging_to(mut self, sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.sink = Some(Arc::new(sink));
        self
    }

    /// The directory the records live in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The record file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// Reads the records.
    ///
    /// A missing file is not a failure: it is the first run, and it returns an
    /// empty book so an empty list is a real result rather than an error.
    ///
    /// # Errors
    ///
    /// Returns the read failure when the file exists but cannot be read, the
    /// corruption when it is not a document this build understands, and the
    /// unsupported-version failure when a newer build wrote it.
    pub fn load(&self) -> Result<HostBook, StorageError> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // "No such file" is only a first run when the data directory is not a
            // file: a root that is a file means every path inside it is unusable,
            // and reporting an empty book there would hide the problem.
            Err(error) if error.kind() == ErrorKind::NotFound && !self.root.is_file() => {
                self.emit(&StoreEvent::Loaded { path, hosts: 0 });
                return Ok(HostBook::default());
            }
            Err(error) => {
                let error = StorageError::Read {
                    path,
                    reason: error.to_string(),
                };
                self.emit_load_failure(&error);
                return Err(error);
            }
        };

        match read_book(&path, &text) {
            Ok(book) => {
                self.emit(&StoreEvent::Loaded {
                    path,
                    hosts: book.hosts().len(),
                });
                Ok(book)
            }
            Err(error) => {
                self.emit_load_failure(&error);
                Err(error)
            }
        }
    }

    /// Writes the records atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write or replace failure when the records could not be
    /// written, and an encode failure when they could not be serialized at all. In
    /// every case the file that was there before is untouched.
    pub fn save(&self, book: &HostBook) -> Result<(), StorageError> {
        let path = self.path();
        match self.write_atomically(&path, book) {
            Ok(()) => {
                self.emit(&StoreEvent::Saved {
                    path,
                    hosts: book.hosts().len(),
                    groups: HostBook::group(book.hosts()).len(),
                    recents: book.recents().len(),
                });
                Ok(())
            }
            Err(error) => {
                self.emit(&StoreEvent::SaveFailed {
                    path,
                    reason: error.to_string(),
                });
                Err(error)
            }
        }
    }

    fn write_atomically(&self, path: &Path, book: &HostBook) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec_pretty(book).map_err(|error| StorageError::Encode {
            reason: error.to_string(),
        })?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::CreateDirectory {
            path: self.root.clone(),
            reason: error.to_string(),
        })?;

        let temp = self.root.join(format!(
            ".{FILE_NAME}.tmp-{}-{}",
            std::process::id(),
            next_temp_sequence()
        ));
        let outcome = write_then_rename(&temp, path, &bytes);
        if outcome.is_err() {
            // Best effort: a leftover temporary file must not turn a failed save
            // into a failed save plus a stray file.
            let _ = std::fs::remove_file(&temp);
        }
        outcome
    }

    fn emit(&self, event: &StoreEvent) {
        if let Some(sink) = &self.sink {
            sink(&event.line());
        }
    }

    fn emit_load_failure(&self, error: &StorageError) {
        let path = self.path();
        self.emit(&StoreEvent::LoadFailed {
            path,
            reason: error.to_string(),
        });
    }
}

/// Writes the bytes to `temp`, flushes them, and then moves the file over `path`.
fn write_then_rename(temp: &Path, path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    {
        use std::io::Write as _;

        let mut file = std::fs::File::create(temp).map_err(|error| StorageError::Write {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| StorageError::Write {
                path: path.to_owned(),
                reason: error.to_string(),
            })?;
    }
    std::fs::rename(temp, path).map_err(|error| StorageError::Replace {
        path: path.to_owned(),
        reason: error.to_string(),
    })
}

/// Reads a document, refusing anything this build does not recognise.
fn read_book(path: &Path, text: &str) -> Result<HostBook, StorageError> {
    /// Only the version, so a newer file is named as such instead of being
    /// reported as damaged because of fields this build has never seen.
    #[derive(Deserialize)]
    struct VersionProbe {
        version: u32,
    }

    let probe: VersionProbe =
        serde_json::from_str(text).map_err(|error| StorageError::Corrupt {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    if probe.version > FORMAT_VERSION {
        return Err(StorageError::UnsupportedVersion {
            path: path.to_owned(),
            found: probe.version,
            supported: FORMAT_VERSION,
        });
    }
    if probe.version == 0 {
        return Err(StorageError::Corrupt {
            path: path.to_owned(),
            reason: "format version 0 was never written by Shellune".to_owned(),
        });
    }

    let book: HostBook = serde_json::from_str(text).map_err(|error| StorageError::Corrupt {
        path: path.to_owned(),
        reason: error.to_string(),
    })?;
    book.ensure_valid()
        .map_err(|reason| StorageError::Corrupt {
            path: path.to_owned(),
            reason,
        })?;
    Ok(book)
}

/// The per-user data directory of this platform.
#[must_use]
pub fn data_directory() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    };
    let base = base.unwrap_or_else(std::env::temp_dir);
    base.join("Shellune")
}

/// A number that makes concurrent temporary-file names unique.
fn next_temp_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}
