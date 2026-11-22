//! The sync settings (`sync.json`) and the sync state (`sync-state.json`).
//!
//! Both live in the data directory beside the other data files, and both are
//! machine state: neither is ever part of the portable document. The settings
//! name the server, the account and the folder; the WebDAV password and the sync
//! passphrase are in the system credential store ([`shellune_storage::AppSecret`]),
//! never in a file. The state remembers what both sides held at the last
//! successful sync, which is what tells a local change from a remote one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shellune_core::{CredentialId, KeyId};

/// The settings file in the data directory.
pub const CONFIG_FILE_NAME: &str = "sync.json";

/// The state file in the data directory.
pub const STATE_FILE_NAME: &str = "sync-state.json";

/// The folder used when none is chosen.
pub const DEFAULT_REMOTE_DIR: &str = "Shellune";

const CONFIG_VERSION: u32 = 1;

/// Where and how this machine syncs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// The WebDAV address of the account (`https://...`).
    pub url: String,
    /// The account's user name; empty when the server needs none.
    pub username: String,
    /// The folder under the address that holds the data file.
    pub remote_dir: String,
    /// Whether changes are uploaded and the cloud is checked without asking.
    pub auto: bool,
    /// Whether the secrets travel too. The field keeps its name for the files
    /// already on machines; its meaning is the vault: with an encrypted vault on
    /// this machine or in the cloud, the document becomes a version 2 one whose
    /// vault block carries the managed keys, the shared passwords and the
    /// accepted server identities, unsealed by the WebDAV password or, on an
    /// account without one, by the sync passphrase (R5). Without a vault
    /// anywhere, this stays the version 1 sealed-password arrangement under the
    /// sync passphrase.
    pub sync_secrets: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            username: String::new(),
            remote_dir: DEFAULT_REMOTE_DIR.to_owned(),
            auto: true,
            sync_secrets: false,
        }
    }
}

impl SyncConfig {
    /// Settings for an account, with the defaults for the rest.
    #[must_use]
    pub fn new(url: impl Into<String>, username: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            username: username.into(),
            ..Self::default()
        }
    }

    /// Whether a server is set at all. Nothing is ever sent without one.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

/// The stored form: a version, then the settings.
#[derive(Serialize, Deserialize)]
struct ConfigFile {
    version: u32,
    #[serde(flatten)]
    config: SyncConfig,
}

/// Why the settings or the state could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read or understood.
    #[error("the sync settings in {path} could not be read: {reason}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The file was written by a newer build.
    #[error("the sync settings use format {found}, but this build reads up to {supported}")]
    UnsupportedVersion {
        /// The version found.
        found: u32,
        /// The newest this build reads.
        supported: u32,
    },
    /// The file could not be written.
    #[error("{path} could not be written: {reason}")]
    Write {
        /// The file.
        path: PathBuf,
        /// Why.
        reason: String,
    },
}

/// `sync.json` in one data directory.
#[derive(Clone, Debug)]
pub struct SyncConfigStore {
    root: PathBuf,
}

impl SyncConfigStore {
    /// The settings file inside `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(CONFIG_FILE_NAME)
    }

    /// The saved settings; `None` when sync was never set up.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Read`] for a damaged or unreadable file and
    /// [`ConfigError::UnsupportedVersion`] for a newer one.
    pub fn load(&self) -> Result<Option<SyncConfig>, ConfigError> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ConfigError::Read {
                    path,
                    reason: error.to_string(),
                });
            }
        };
        let read_error = |reason: String| ConfigError::Read {
            path: path.clone(),
            reason,
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| read_error(error.to_string()))?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| read_error("the format version is missing".to_owned()))?;
        if version > CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: version,
                supported: CONFIG_VERSION,
            });
        }
        let file: ConfigFile =
            serde_json::from_value(value).map_err(|error| read_error(error.to_string()))?;
        Ok(Some(file.config))
    }

    /// Saves the settings atomically.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Write`] when the file could not be written; the
    /// previous file is left as it was.
    pub fn save(&self, config: &SyncConfig) -> Result<(), ConfigError> {
        let bytes = serde_json::to_vec_pretty(&ConfigFile {
            version: CONFIG_VERSION,
            config: config.clone(),
        })
        .map_err(|error| ConfigError::Write {
            path: self.path(),
            reason: error.to_string(),
        })?;
        write_atomic(&self.root, CONFIG_FILE_NAME, &bytes)
    }
}

/// How the last sync ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LastResult {
    /// Both sides already held the same data.
    UpToDate,
    /// This machine's data was uploaded.
    Uploaded,
    /// The cloud's data was applied here.
    Pulled,
    /// Both sides changed; the user has not chosen yet.
    Conflict,
    /// The sync failed; nothing local was lost.
    Failed {
        /// The reason, as shown to the user.
        reason: String,
    },
}

/// What this machine remembers about the last sync.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    /// The remote file the rest is about. Another server or folder starts over,
    /// as a first sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// The remote file's entity tag after the last successful sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_remote_etag: Option<String>,
    /// The fingerprint of the remote document as this platform sees it (with its
    /// sealed password block when passwords are synced), after the last
    /// successful sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_remote_hash: Option<String>,
    /// The fingerprint of the local data (shared part and this platform's
    /// section) after the last successful sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_hash: Option<String>,
    /// The fingerprint of the same data without the recent-connection log
    /// ([`shellune_storage::ShelluneData::core_view`]) after the last successful
    /// sync. With it a side that only connected to hosts is told from one that
    /// changed records, and the two are merged instead of reported as a conflict;
    /// a state written before it existed keeps the plain conflict rule until the
    /// next successful sync records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_core_hash: Option<String>,
    /// The fingerprint of the cloud document's view for this platform without
    /// the recent-connection log, as the cloud held it after the last successful
    /// sync (downloaded, uploaded or merged). The cloud is compared with this,
    /// not with the local data: the two views differ whenever another platform
    /// wrote the cloud file last, so comparing across them would take a cloud
    /// that only connected to hosts for one that changed records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_remote_core_hash: Option<String>,
    /// A keyed fingerprint of the saved passwords as they were last synced; `None`
    /// when passwords were not synced. Changing a password, the passphrase, or the
    /// choice to sync passwords is a local change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_secrets_hash: Option<String>,
    /// The identity of this machine as a writer of vault records (ADR-5): the
    /// device name stamped into the record version every merge writes. Drawn once
    /// and kept, so two syncs of one machine never look like two devices.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// The fingerprint of the public part of this machine's encrypted vault as it
    /// was after the last successful sync; `None` when no vault travelled or the
    /// vault did not travel then. A record, tombstone or trust change is a local
    /// change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_vault_hash: Option<String>,
    /// The fingerprint of the public part of the vault the cloud document carried
    /// after the last successful sync; `None` when it carried none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_remote_vault_hash: Option<String>,
    /// The record-version baseline of the encrypted vault at the last successful
    /// sync (ADR-5): what tells "only this machine changed the record" from "both
    /// did", and what the global counter of an abnormally rolled-back cloud is
    /// compared against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultBaseline>,
    /// How the last attempt ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<LastResult>,
    /// When the last attempt ended, in seconds since the Unix epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_time_unix: Option<u64>,
    /// When the last successful sync ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_unix: Option<u64>,
}

impl SyncState {
    /// Whether a sync with `remote` ever succeeded, so changes can be told apart.
    #[must_use]
    pub fn knows(&self, remote: &str) -> bool {
        self.remote.as_deref() == Some(remote) && self.last_synced_hash.is_some()
    }

    /// Whether the last attempt left a conflict waiting for the user.
    #[must_use]
    pub fn has_conflict(&self) -> bool {
        self.last_result == Some(LastResult::Conflict)
    }
}

/// One vault record as the last successful sync saw it: which version it carried
/// and whether it was already a tombstone. A deletion keeps the version it had,
/// so the liveness beside the version is what tells a deletion from a stasis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRecordBase {
    /// How many changes the record had had.
    pub counter: u64,
    /// Which device wrote that version.
    pub device_id: String,
    /// Whether the record was already deleted at the last sync.
    pub deleted: bool,
}

/// The common baseline of the last successful vault sync (ADR-5): the version of
/// every record both sides held, the global counter of the whole vault and the
/// accepted server identities. It holds public metadata only — names,
/// algorithms, fingerprints — never a secret; the passwords themselves never
/// leave the ciphertext.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultBaseline {
    /// The identity of the vault the records belong to.
    pub vault_id: String,
    /// The highest record version counter of that state; a cloud vault below it
    /// is an abnormal rollback.
    pub global_counter: u64,
    /// Every managed key, live and deleted.
    pub keys: BTreeMap<KeyId, VaultRecordBase>,
    /// Every shared credential, live and deleted.
    pub credentials: BTreeMap<CredentialId, VaultRecordBase>,
    /// Every accepted server identity, as `host:port` to fingerprint.
    pub trust: BTreeMap<String, String>,
}

/// `sync-state.json` in one data directory.
#[derive(Clone, Debug)]
pub struct SyncStateStore {
    root: PathBuf,
}

impl SyncStateStore {
    /// The state file inside `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(STATE_FILE_NAME)
    }

    /// The saved state. A missing or damaged file reads as no state, which makes
    /// the next sync a first sync: it never overwrites either side without asking
    /// when both hold data.
    #[must_use]
    pub fn load(&self) -> SyncState {
        std::fs::read(self.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Saves the state atomically.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Write`] when the file could not be written.
    pub fn save(&self, state: &SyncState) -> Result<(), ConfigError> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| ConfigError::Write {
            path: self.path(),
            reason: error.to_string(),
        })?;
        write_atomic(&self.root, STATE_FILE_NAME, &bytes)
    }
}

/// Writes a file through a temporary file in the same directory and a rename.
pub(crate) fn write_atomic(root: &Path, name: &str, bytes: &[u8]) -> Result<(), ConfigError> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let path = root.join(name);
    let error = |reason: String| ConfigError::Write {
        path: path.clone(),
        reason,
    };
    std::fs::create_dir_all(root).map_err(|failure| error(failure.to_string()))?;
    let temp = root.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let outcome = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &path)
    })();
    if let Err(failure) = outcome {
        let _ = std::fs::remove_file(&temp);
        return Err(error(failure.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shellune-sync-config-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_default_to_automatic_sync_without_passwords() {
        let config = SyncConfig::default();
        assert!(config.auto);
        assert!(!config.sync_secrets);
        assert!(!config.is_configured());
        assert_eq!(config.remote_dir, DEFAULT_REMOTE_DIR);
        // A file that names only the address takes the same defaults.
        let file: ConfigFile =
            serde_json::from_str(r#"{"version":1,"url":"https://dav"}"#).expect("reads");
        assert!(file.config.auto);
        assert!(!file.config.sync_secrets);
        assert!(file.config.is_configured());
    }

    #[test]
    fn settings_round_trip_and_a_newer_or_damaged_file_is_reported() {
        let root = scratch("round-trip");
        let store = SyncConfigStore::at(&root);
        assert_eq!(store.load(), Ok(None));
        let mut config = SyncConfig::new("https://dav.example.com/", "me");
        config.auto = false;
        config.sync_secrets = true;
        store.save(&config).expect("saved");
        assert_eq!(store.load(), Ok(Some(config)));

        std::fs::write(store.path(), br#"{"version":9,"url":"x"}"#).expect("written");
        assert!(matches!(
            store.load(),
            Err(ConfigError::UnsupportedVersion { found: 9, .. })
        ));
        std::fs::write(store.path(), b"{not json").expect("written");
        assert!(matches!(store.load(), Err(ConfigError::Read { .. })));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_damaged_state_reads_as_a_first_sync() {
        let root = scratch("state");
        let store = SyncStateStore::at(&root);
        assert_eq!(store.load(), SyncState::default());
        let state = SyncState {
            remote: Some("https://dav/x/shellune-data.json".to_owned()),
            last_remote_etag: Some("\"e1\"".to_owned()),
            last_remote_hash: Some("aa".to_owned()),
            last_synced_hash: Some("bb".to_owned()),
            last_synced_core_hash: Some("cc".to_owned()),
            last_remote_core_hash: Some("dd".to_owned()),
            last_secrets_hash: None,
            last_result: Some(LastResult::Failed {
                reason: "offline".to_owned(),
            }),
            last_time_unix: Some(5),
            last_success_unix: Some(4),
            ..SyncState::default()
        };
        store.save(&state).expect("saved");
        assert_eq!(store.load(), state);
        assert!(state.knows("https://dav/x/shellune-data.json"));
        assert!(!state.knows("https://other/shellune-data.json"));

        // The vault baseline travels in the same file, and a state an older
        // build wrote simply has none of it.
        let mut with_vault = state.clone();
        with_vault.device_id = Some("device-a".to_owned());
        with_vault.vault = Some(VaultBaseline {
            vault_id: "0123456789abcdef".to_owned(),
            global_counter: 7,
            keys: BTreeMap::from([(
                KeyId::new(),
                VaultRecordBase {
                    counter: 7,
                    device_id: "device-a".to_owned(),
                    deleted: false,
                },
            )]),
            credentials: BTreeMap::from([(
                CredentialId::new(),
                VaultRecordBase {
                    counter: 3,
                    device_id: "device-b".to_owned(),
                    deleted: true,
                },
            )]),
            trust: BTreeMap::from([("web:22".to_owned(), "SHA256:old".to_owned())]),
        });
        store.save(&with_vault).expect("saved");
        assert_eq!(store.load(), with_vault);
        assert!(state.vault.is_none(), "the older state keeps its shape");
        // A state an older build wrote has no fingerprint without recents.
        std::fs::write(
            store.path(),
            br#"{"remote":"https://dav/x/shellune-data.json","last_synced_hash":"bb"}"#,
        )
        .expect("written");
        let older = store.load();
        assert_eq!(older.last_synced_hash.as_deref(), Some("bb"));
        assert_eq!(older.last_synced_core_hash, None);
        assert_eq!(older.last_remote_core_hash, None);
        std::fs::write(store.path(), b"garbage").expect("written");
        assert_eq!(store.load(), SyncState::default());
        let _ = std::fs::remove_dir_all(root);
    }
}
