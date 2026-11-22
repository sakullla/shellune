//! The operating system each saved host was last seen running.
//!
//! This is a cache of what servers said about themselves, not a choice anybody made,
//! so it lives in a file of its own rather than in the host records: the hosts file
//! keeps the shape every build can read (its records refuse fields they do not
//! know), and losing this file only means the marks come back on the next
//! connection. Entries are short identifiers from `shellune_ssh::platform::KNOWN`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shellune_core::HostId;

use crate::error::StorageError;
use crate::store::write_atomic_file;

/// The cache's file name inside the data directory.
pub const FILE_NAME: &str = "platforms.json";

/// The longest identifier kept; anything longer is not one this build wrote.
const ID_LIMIT: usize = 32;

#[derive(Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    hosts: BTreeMap<HostId, String>,
}

/// Reads and writes the platform cache.
pub struct PlatformStore {
    root: PathBuf,
}

impl PlatformStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The cache file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// The directory the file lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every host's last seen system. A missing or unreadable file is an empty cache.
    #[must_use]
    pub fn load(&self) -> BTreeMap<HostId, String> {
        let Ok(text) = std::fs::read_to_string(self.path()) else {
            return BTreeMap::new();
        };
        serde_json::from_str::<Document>(&text)
            .map(|document| {
                document
                    .hosts
                    .into_iter()
                    .filter(|(_, id)| valid(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Writes the whole cache.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the file cannot be written.
    pub fn save(&self, hosts: &BTreeMap<HostId, String>) -> Result<(), StorageError> {
        let document = Document {
            hosts: hosts
                .iter()
                .filter(|(_, id)| valid(id))
                .map(|(host, id)| (*host, id.clone()))
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|error| StorageError::Encode {
            reason: error.to_string(),
        })?;
        write_atomic_file(&self.root, FILE_NAME, &bytes)
    }
}

/// Whether a stored identifier is one this build could have written.
fn valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_LIMIT
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shellune-platforms-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn a_saved_cache_loads_back_and_a_missing_one_is_empty() {
        let dir = scratch("round-trip");
        let store = PlatformStore::at(&dir);
        assert!(store.load().is_empty());
        let host = HostId::new();
        let mut hosts = BTreeMap::new();
        hosts.insert(host, "debian".to_owned());
        store.save(&hosts).expect("saves");
        assert_eq!(store.load().get(&host).map(String::as_str), Some("debian"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_or_a_strange_entry_is_ignored() {
        let dir = scratch("damaged");
        let store = PlatformStore::at(&dir);
        std::fs::write(store.path(), "{ not json").expect("writes");
        assert!(store.load().is_empty());
        let host = HostId::new();
        std::fs::write(
            store.path(),
            format!(r#"{{"hosts":{{"{host}":"<script>"}}}}"#),
        )
        .expect("writes");
        assert!(
            store.load().is_empty(),
            "only identifiers this build writes are kept"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
