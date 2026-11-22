//! The vault file of this machine: strict loading, atomic saving, encrypted
//! snapshots and the migration out of the version 1 portable document
//! (R2, R9).
//!
//! `vault.json` is the whole ciphertext (see [`VaultCipher`]) in a small
//! envelope that names the format, exactly the way the other data files are
//! versioned. Reading is strict — a damaged file or one of a newer format is
//! reported, never guessed at — and writing is atomic, so a crash, a full disk
//! or a permission error leaves the previous file exactly as it was.
//!
//! Before the file is replaced, the previous ciphertext is copied into the
//! snapshot directory the portable data already keeps: the copy is the restore
//! point, and because it is the ciphertext and not the records, it holds no
//! secret in the clear.
//!
//! The migration is the read-only half of the version 1 format: the sealed
//! passwords of a version 1 document and the accepted identities of its shared
//! part move into a new vault, and nothing else is touched. The source file,
//! the host records and the old system passwords stay exactly as they were —
//! deleting those is a separate, later decision of the user — and a failed
//! migration writes no file at all, so a retry cannot double a record.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shellune_core::{
    ConnectionRecord, CredentialId, CredentialMeta, HostConfig, SecretString, VaultRecordVersion,
};

use crate::credentials::{AppSecret, SecretStore};
use crate::portable::{KdfParams, PortableError, ShelluneData, SnapshotStore};
use crate::store::write_atomic_file;

use super::content::{HostReference, VaultContent, VaultCredentialRecord};
use super::crypto::{self, KeySource, PassphrasePurpose, VaultCipher};

/// Name of the vault file inside the data directory.
pub const VAULT_FILE_NAME: &str = "vault.json";

/// What tells the vault file from any other JSON.
pub const VAULT_FORMAT: &str = "shellune-vault";

/// The format this build writes and the newest it reads.
///
/// There is no version 1 of this file: the version 1 format is the portable
/// data document, whose sealed passwords and shared trust records are the only
/// things this build migrates from. An older build of Shellune never meets
/// `vault.json`; an old client meets a version 2 portable document and refuses
/// it by the version check it already has, which is what keeps the new secrets
/// from being downgraded or dropped.
pub const VAULT_FORMAT_VERSION: u32 = 2;

/// The snapshot reason of a vault save, so the copies are named in the folder
/// the portable data already keeps.
const SNAPSHOT_REASON: &str = "vault-save";

/// How the caller wants a vault to be openable — the four secrets of R2.
#[derive(Clone, Debug)]
pub enum UnsealSource {
    /// The random local secret only this machine's system credential store
    /// holds. Generated when this machine has none yet; never shown, copied
    /// or exported.
    LocalMachine,
    /// The WebDAV account password, reused to seal the syncing vault so a
    /// configured WebDAV needs no second sync passphrase.
    WebDavPassword(SecretString),
    /// The sync passphrase, which a passwordless WebDAV and every local
    /// export require and share.
    SyncPassphrase(SecretString),
}

impl UnsealSource {
    fn check_not_empty(&self) -> Result<(), VaultStoreError> {
        let (blank, which) = match self {
            Self::LocalMachine => (false, ""),
            Self::WebDavPassword(secret) => (secret.expose().trim().is_empty(), "WebDAV password"),
            Self::SyncPassphrase(secret) => (secret.expose().trim().is_empty(), "sync passphrase"),
        };
        if blank {
            return Err(VaultStoreError::EmptySecret { which });
        }
        Ok(())
    }
}

/// What a migration out of a version 1 document moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    /// How many saved passwords became shared credential records.
    pub credentials: usize,
    /// How many accepted server identities moved into the vault.
    pub trust_entries: usize,
}

/// Why the vault could not be opened, saved or migrated.
///
/// No variant carries a secret: the reasons name files, fields and counts.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VaultStoreError {
    /// There is no vault file where one was needed.
    #[error("there is no encrypted vault at {path} yet")]
    Missing {
        /// The file that is not there.
        path: PathBuf,
    },
    /// A vault was to be created where one already exists.
    #[error("an encrypted vault already exists at {path}")]
    AlreadyExists {
        /// The file that is there.
        path: PathBuf,
    },
    /// The file exists but could not be read.
    #[error("the encrypted vault at {path} could not be read: {reason}")]
    Read {
        /// The file that was read.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The file is not a vault this build understands.
    #[error("the encrypted vault is not valid: {reason}")]
    Corrupt {
        /// What could not be understood.
        reason: String,
    },
    /// The file was written by a newer build and was left as it was.
    #[error(
        "the encrypted vault at {path} uses format {found}, but this build reads up to {supported}; the file was left unchanged"
    )]
    UnsupportedVersion {
        /// The file that was read.
        path: PathBuf,
        /// The format version found in the file.
        found: u32,
        /// The newest format this build understands.
        supported: u32,
    },
    /// The vault could not be opened: the secret is wrong or the sealed bytes
    /// were changed. The two cannot be told apart.
    #[error("the vault could not be opened: the secret is wrong or the sealed bytes were changed")]
    Sealed,
    /// An empty secret was given where a non-empty one is required.
    #[error("the {which} is empty")]
    EmptySecret {
        /// Which secret was empty.
        which: &'static str,
    },
    /// This machine holds no local unseal secret in its system credential
    /// store. An unread store answers differently, so "no entry" is never
    /// mistaken for "store broken".
    #[error("this machine holds no local unseal secret in its system credential store")]
    NoLocalSecret,
    /// The system credential store failed.
    #[error("the system credential store {reason}")]
    SystemStore {
        /// What failed, without any secret it may have been asked for.
        reason: String,
    },
    /// The write was refused before anything changed.
    #[error("the vault refused the write: {reason}")]
    Invalid {
        /// What would have been left inconsistent.
        reason: String,
    },
    /// The system random source failed.
    #[error("the system random source failed: {reason}")]
    RandomSource {
        /// Why.
        reason: String,
    },
    /// The records could not be encrypted.
    #[error("the vault could not be encrypted: {reason}")]
    Encrypt {
        /// Why.
        reason: String,
    },
    /// The records could not be encoded at all.
    #[error("the vault could not be encoded: {reason}")]
    Encode {
        /// What the serializer reported.
        reason: String,
    },
    /// The encrypted copy taken before a write could not be made; nothing was
    /// changed.
    #[error(
        "a copy of the encrypted vault could not be made in {path}: {reason}. Nothing was changed"
    )]
    Snapshot {
        /// The snapshot directory.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The file could not be written; the previous file is untouched.
    #[error("the encrypted vault at {path} could not be written: {reason}")]
    Write {
        /// The file that was being written.
        path: PathBuf,
        /// Why.
        reason: String,
    },
}

/// The envelope of `vault.json`: the format name, the version, and the whole
/// ciphertext. Keeping the envelope apart from the cipher is what lets the
/// same ciphertext travel inside a portable document without carrying a
/// version of its own.
#[derive(Serialize, Deserialize)]
struct VaultFile {
    format: String,
    version: u32,
    cipher: VaultCipher,
}

/// A vault file at a known location.
#[derive(Clone, Debug)]
pub struct VaultStore {
    root: PathBuf,
}

impl VaultStore {
    /// A vault file inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A vault file in the per-user data directory of this platform.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(crate::store::data_directory())
    }

    /// The directory the vault file lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The vault file itself.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(VAULT_FILE_NAME)
    }

    /// Whether a vault file exists. A `true` here says nothing about the file:
    /// opening it is the only way to know it is usable.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.path().exists()
    }

    /// The sealed vault as it stands, without any secret: `Ok(None)` when this
    /// machine has no vault file yet. Useful for asking what a vault can be
    /// opened with before asking the user for anything.
    ///
    /// # Errors
    ///
    /// Returns the read failure, the corruption and the unsupported-version
    /// failure of [`VaultStoreError`]; the file is never rewritten.
    pub fn read_cipher(&self) -> Result<Option<VaultCipher>, VaultStoreError> {
        Ok(self.read_previous()?.map(|(_, cipher, _)| cipher))
    }

    /// Opens the vault with one source.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Missing`] when there is no vault file,
    /// [`VaultStoreError::Sealed`] when the secret is wrong or the sealed
    /// bytes were changed, and the system-store failures of the local secret.
    /// Nothing is written; opening cannot change the last good file.
    pub fn open(
        &self,
        source: &UnsealSource,
        secrets: &dyn SecretStore,
    ) -> Result<VaultContent, VaultStoreError> {
        let cipher = self
            .read_cipher()?
            .ok_or_else(|| VaultStoreError::Missing { path: self.path() })?;
        let key_source = self.key_source(source, secrets)?;
        cipher.open(&key_source)
    }

    /// Saves `content` as a new ciphertext sealed for every source, all or
    /// nothing, validated against a book of SSH hosts alone.
    ///
    /// The book a build without desktop references kept; the whole-book save is
    /// [`Self::save_records`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::save_records`].
    pub fn save(
        &self,
        content: &VaultContent,
        hosts: &[HostConfig],
        sources: &[UnsealSource],
        kdf: KdfParams,
        secrets: &dyn SecretStore,
    ) -> Result<(), VaultStoreError> {
        let records: Vec<ConnectionRecord> =
            hosts.iter().cloned().map(ConnectionRecord::Ssh).collect();
        self.save_records(content, &records, sources, kdf, secrets)
    }

    /// Saves `content` as a new ciphertext sealed for every source, all or
    /// nothing, validated against a whole connection book.
    ///
    /// The content is validated against every record the book holds first — the
    /// core catalog checks over the SSH hosts, the sign-in-method rule, the
    /// desktop references and the reference snapshot — so no write can leave a
    /// record pointing at nothing. When a vault already exists, its identity is
    /// kept (the wraps stay bound to their vault) and its ciphertext is copied
    /// aside as an encrypted snapshot before the new file replaces it. A local
    /// secret this machine does not have yet is stored in the system credential
    /// store *before* the file is written and removed again when the write
    /// fails, so no failure leaves a vault no secret can open, or an orphan
    /// entry in the store.
    ///
    /// The sources name every wrap the new ciphertext will have; a source the
    /// old file needed but the caller does not name is dropped, which is how a
    /// password change re-seals a vault, and a caller that names none at all
    /// hears about it here rather than from a vault nothing can open.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] for content the connection book
    /// contradicts, [`VaultStoreError::EmptySecret`] for a blank passphrase,
    /// and [`VaultStoreError::Corrupt`] or [`VaultStoreError::UnsupportedVersion`]
    /// when an existing file cannot be read — the last good file is never
    /// overwritten by a save that could not read it — and the snapshot, system
    /// store and write failures. In every case the previous file is intact.
    pub fn save_records(
        &self,
        content: &VaultContent,
        records: &[ConnectionRecord],
        sources: &[UnsealSource],
        kdf: KdfParams,
        secrets: &dyn SecretStore,
    ) -> Result<(), VaultStoreError> {
        if sources.is_empty() {
            return Err(VaultStoreError::Invalid {
                reason: "a vault must be sealed for at least one way to unseal it".to_owned(),
            });
        }
        for source in sources {
            source.check_not_empty()?;
        }
        content.validate_for_records(records)?;

        let previous = self.read_previous()?;
        let vault_id = previous.as_ref().map(|(id, _, _)| id.clone());
        let previous_bytes = previous.map(|(_, _, bytes)| bytes);

        // A local secret this machine lacks is created before anything else,
        // so a later failure has something to undo and nothing depends on a
        // store entry that was never written.
        //
        // One machine has one local secret, so a caller that names it twice
        // names it once: two wraps of the same secret would double the local
        // entry in the file, and the reader refuses a vault that does — which
        // would make this build unable to open the very file it wrote. The
        // passphrases are not deduped: several wraps of one purpose are a
        // legitimate state the reader accepts.
        let mut created_local_secret = false;
        let mut local_source_seen = false;
        let mut key_sources = Vec::with_capacity(sources.len());
        for source in sources {
            match source {
                UnsealSource::LocalMachine if local_source_seen => {}
                UnsealSource::LocalMachine => {
                    local_source_seen = true;
                    let (key_source, created) = self.local_key_source(secrets)?;
                    created_local_secret |= created;
                    key_sources.push(key_source);
                }
                UnsealSource::WebDavPassword(secret) => {
                    key_sources.push(KeySource::Passphrase {
                        purpose: PassphrasePurpose::WebDavPassword,
                        value: secret.clone(),
                    });
                }
                UnsealSource::SyncPassphrase(secret) => {
                    key_sources.push(KeySource::Passphrase {
                        purpose: PassphrasePurpose::SyncPassphrase,
                        value: secret.clone(),
                    });
                }
            }
        }

        // Sealing sits inside the rollback on purpose: if it fails after this
        // save created a local unseal secret, the entry would be a secret for
        // a ciphertext that was never written, so it is removed again.
        let outcome = (|| {
            let cipher = VaultCipher::seal(content, vault_id.as_deref(), &key_sources, kdf)?;
            if let Some(bytes) = &previous_bytes {
                self.snapshot_previous(bytes)?;
            }
            self.write(&cipher)
        })();
        if outcome.is_err() && created_local_secret {
            let _ = secrets.forget_app(AppSecret::VaultLocalUnseal);
        }
        outcome
    }

    /// Migrates the sealed passwords and the accepted identities of a version
    /// 1 portable document into a new vault of this machine (R9).
    ///
    /// The migration is read-only for everything but the new vault file: the
    /// source document, the host records and the old system passwords are left
    /// exactly as they were, and the saved passwords of the document become
    /// shared credential records — one per host whose password travelled —
    /// that no host references yet, because binding a host to a vault record
    /// is the application's next, visible step.
    ///
    /// It refuses when a vault already exists (a retry after a failure finds
    /// no file and cannot double a record), when the document is not version
    /// 1, and when the passphrase cannot open the sealed passwords. On every
    /// failure nothing is written at all.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::save`] and
    /// [`VaultStoreError::AlreadyExists`]; a failed migration leaves no vault
    /// file, no system-store entry and no half-migrated record.
    pub fn migrate_from_v1(
        &self,
        document: &ShelluneData,
        passphrase: Option<&SecretString>,
        hosts: &[HostConfig],
        device_id: &str,
        secrets: &dyn SecretStore,
    ) -> Result<MigrationReport, VaultStoreError> {
        if self.exists() {
            return Err(VaultStoreError::AlreadyExists { path: self.path() });
        }
        if document.version() != 1 {
            return Err(VaultStoreError::Invalid {
                reason: format!(
                    "a version {} document is not the version 1 format that is migrated",
                    document.version()
                ),
            });
        }

        let mut content = VaultContent::new();
        content.set_trust(document.shared().known_hosts().clone());
        if document.has_secrets() {
            let passphrase = passphrase.ok_or(VaultStoreError::EmptySecret {
                which: "sync passphrase of the version 1 document",
            })?;
            if passphrase.expose().trim().is_empty() {
                return Err(VaultStoreError::EmptySecret {
                    which: "sync passphrase of the version 1 document",
                });
            }
            let bundle = document
                .open_secrets(passphrase)
                .map_err(|error| migration_error(error, &self.path()))?;
            let records = document.shared().hosts().records();
            for host_id in bundle.ids() {
                let (name, user) = records
                    .iter()
                    .find_map(|record| match record {
                        ConnectionRecord::Ssh(host) if host.id() == host_id => {
                            Some((host.name().to_owned(), Some(host.user().to_owned())))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| ("migrated password".to_owned(), None));
                let secret = bundle
                    .get(host_id)
                    .cloned()
                    .unwrap_or_else(|| SecretString::new(""));
                content
                    .insert_credential(VaultCredentialRecord::new(
                        CredentialMeta::new(
                            CredentialId::new(),
                            name,
                            user,
                            VaultRecordVersion::new(1, device_id),
                        ),
                        secret,
                    ))
                    .map_err(|error| VaultStoreError::Corrupt {
                        reason: error.to_string(),
                    })?;
            }
        }
        content.set_references(&HostReference::snapshot(hosts));
        let report = MigrationReport {
            credentials: content
                .credentials()
                .filter(|record| record.meta().is_live())
                .count(),
            trust_entries: content.trust().entries().len(),
        };
        self.save(
            &content,
            hosts,
            &[UnsealSource::LocalMachine],
            KdfParams::default(),
            secrets,
        )?;
        Ok(report)
    }

    /// The previous file, parsed and as bytes, when there is one. A file this
    /// build cannot read is an error, not an empty vault: overwriting it would
    /// destroy the last ciphertext on the grounds that it could not be opened.
    fn read_previous(&self) -> Result<Option<(String, VaultCipher, Vec<u8>)>, VaultStoreError> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(VaultStoreError::Read {
                    path,
                    reason: error.to_string(),
                });
            }
        };
        let file = parse_vault_file(&path, &bytes)?;
        Ok(Some((
            file.cipher.vault_id().to_owned(),
            file.cipher,
            bytes,
        )))
    }

    /// Copies the previous ciphertext into the snapshot directory. The copy is
    /// the ciphertext itself, so it is a restore point that holds no secret in
    /// the clear.
    fn snapshot_previous(&self, bytes: &[u8]) -> Result<(), VaultStoreError> {
        let store = SnapshotStore::at(&self.root);
        store
            .create(
                SNAPSHOT_REASON,
                crate::portable::now_unix(),
                &[(VAULT_FILE_NAME, bytes)],
            )
            .map_err(|error| match error {
                PortableError::Snapshot { path, reason } => {
                    VaultStoreError::Snapshot { path, reason }
                }
                other => VaultStoreError::Snapshot {
                    path: store.directory(),
                    reason: other.to_string(),
                },
            })
            .map(|_| ())
    }

    fn write(&self, cipher: &VaultCipher) -> Result<(), VaultStoreError> {
        // The last gate before the file: nothing this build refuses to read
        // back may be written, because a vault that cannot be opened by its
        // own writer is locked for good — later saves keep the last good file
        // and refuse to overwrite what they cannot read.
        cipher
            .validate()
            .map_err(|reason| VaultStoreError::Invalid { reason })?;
        let file = VaultFile {
            format: VAULT_FORMAT.to_owned(),
            version: VAULT_FORMAT_VERSION,
            cipher: cipher.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|error| VaultStoreError::Encode {
            reason: error.to_string(),
        })?;
        write_atomic_file(&self.root, VAULT_FILE_NAME, &bytes).map_err(|error| {
            VaultStoreError::Write {
                path: self.path(),
                reason: error.to_string(),
            }
        })
    }

    /// The local unseal secret of this machine, without touching the store:
    /// opening never creates, so a machine without a local secret is reported
    /// as such instead of quietly growing an entry.
    fn key_source(
        &self,
        source: &UnsealSource,
        secrets: &dyn SecretStore,
    ) -> Result<KeySource, VaultStoreError> {
        match source {
            UnsealSource::LocalMachine => match secrets.load_app(AppSecret::VaultLocalUnseal) {
                Ok(Some(secret)) => Ok(KeySource::LocalMachine(secret)),
                Ok(None) => Err(VaultStoreError::NoLocalSecret),
                Err(error) => Err(VaultStoreError::SystemStore {
                    reason: error.to_string(),
                }),
            },
            UnsealSource::WebDavPassword(secret) => Ok(KeySource::Passphrase {
                purpose: PassphrasePurpose::WebDavPassword,
                value: secret.clone(),
            }),
            UnsealSource::SyncPassphrase(secret) => Ok(KeySource::Passphrase {
                purpose: PassphrasePurpose::SyncPassphrase,
                value: secret.clone(),
            }),
        }
    }

    /// The local unseal secret, creating one for this machine when it has
    /// none. The entry is stored before the caller writes anything, and the
    /// second half of the pair says whether that happened, so a later failure
    /// can undo exactly this.
    fn local_key_source(
        &self,
        secrets: &dyn SecretStore,
    ) -> Result<(KeySource, bool), VaultStoreError> {
        match secrets.load_app(AppSecret::VaultLocalUnseal) {
            Ok(Some(secret)) => Ok((KeySource::LocalMachine(secret), false)),
            Ok(None) => {
                // Stored first: a vault whose local secret was never saved
                // would be a ciphertext no secret of this machine can open.
                let secret = crypto::generate_local_secret()?;
                secrets
                    .save_app(AppSecret::VaultLocalUnseal, &secret)
                    .map_err(|error| VaultStoreError::SystemStore {
                        reason: error.to_string(),
                    })?;
                Ok((KeySource::LocalMachine(secret), true))
            }
            Err(error) => Err(VaultStoreError::SystemStore {
                reason: error.to_string(),
            }),
        }
    }
}

/// Reads a vault file, refusing anything this build does not recognise.
fn parse_vault_file(path: &Path, bytes: &[u8]) -> Result<VaultFile, VaultStoreError> {
    let corrupt = |reason: String| VaultStoreError::Corrupt { reason };
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| corrupt(error.to_string()))?;
    let Some(object) = value.as_object() else {
        return Err(corrupt("the vault file is not a JSON object".to_owned()));
    };
    if object.get("format").and_then(serde_json::Value::as_str) != Some(VAULT_FORMAT) {
        return Err(corrupt(format!(
            "the \"format\" field of the vault file is not \"{VAULT_FORMAT}\""
        )));
    }
    let version = object
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| corrupt("the vault file has no readable format version".to_owned()))?;
    if version > VAULT_FORMAT_VERSION {
        return Err(VaultStoreError::UnsupportedVersion {
            path: path.to_owned(),
            found: version,
            supported: VAULT_FORMAT_VERSION,
        });
    }
    if version != VAULT_FORMAT_VERSION {
        return Err(corrupt(format!(
            "vault format {version} was never written by Shellune; the version 1 format is the portable data file"
        )));
    }
    let file: VaultFile =
        serde_json::from_value(value).map_err(|error| corrupt(error.to_string()))?;
    file.cipher.validate().map_err(corrupt)?;
    Ok(file)
}

/// Turns the failures of opening a version 1 document into the failures of the
/// migration, without carrying anything the sealed block might have held.
fn migration_error(error: PortableError, path: &Path) -> VaultStoreError {
    match error {
        PortableError::WrongPassphrase => VaultStoreError::Sealed,
        PortableError::EmptyPassphrase => VaultStoreError::EmptySecret {
            which: "sync passphrase of the version 1 document",
        },
        PortableError::Corrupt { reason } => VaultStoreError::Corrupt { reason },
        other => VaultStoreError::Read {
            path: path.to_owned(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_of_a_newer_or_invented_format_is_named_as_such() {
        let file = VaultFile {
            format: VAULT_FORMAT.to_owned(),
            version: VAULT_FORMAT_VERSION,
            cipher: VaultCipher::seal(
                &VaultContent::new(),
                Some("0123456789abcdef0123456789abcdef"),
                &[KeySource::LocalMachine(
                    crypto::generate_local_secret().expect("secret"),
                )],
                KdfParams::new(64, 1, 1).expect("parameters"),
            )
            .expect("seals"),
        };
        let bytes = serde_json::to_vec(&file).expect("serializes");
        parse_vault_file(Path::new("vault.json"), &bytes).expect("this build's file reads");

        let mut newer: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        newer["version"] = serde_json::json!(VAULT_FORMAT_VERSION + 1);
        assert!(matches!(
            parse_vault_file(Path::new("vault.json"), &serde_json::to_vec(&newer).expect("json")),
            Err(VaultStoreError::UnsupportedVersion { found, .. }) if found == VAULT_FORMAT_VERSION + 1
        ));

        let mut invented: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        invented["version"] = serde_json::json!(1);
        assert!(matches!(
            parse_vault_file(
                Path::new("vault.json"),
                &serde_json::to_vec(&invented).expect("json")
            ),
            Err(VaultStoreError::Corrupt { .. })
        ));

        let mut foreign: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        foreign["format"] = serde_json::json!("shellune-data");
        assert!(matches!(
            parse_vault_file(
                Path::new("vault.json"),
                &serde_json::to_vec(&foreign).expect("json")
            ),
            Err(VaultStoreError::Corrupt { .. })
        ));
    }
}
