//! The portable data file: everything a user would carry to another machine, in
//! one document (R5, R6, R9, R10).
//!
//! A [`ShelluneData`] document is what a backup writes and what sync uploads. It
//! has three parts:
//!
//! * `shared` — the records that mean the same on every machine: the host book
//!   (hosts, other connections, recents, favourites), the group tree, the
//!   forwarding rules, the saved commands, the accepted server identities and the
//!   preferences that are not about the machine.
//! * `platforms.<os>` — what only makes sense on one kind of machine: that
//!   platform's key bindings, its text size and sidebar choice, and for each host
//!   the private-key path, serial device and RDP mapped folder it uses there. A machine writes only
//!   its own section; the sections of the other platforms are carried over from
//!   the document it started from, so a Windows key path survives a round trip
//!   through a Linux machine.
//! * `secrets` — optional in a version 1 document: the saved passwords, sealed
//!   under a passphrase (see [`KdfParams`]). Without it the file holds no
//!   password at all. The passwords it seals are read from the vault, and the
//!   passwords it brings are written into the vault (ADR-6): an export or an
//!   import never asks the system credential store for a host's entry.
//! * `vault` — optional in a version 2 document: the whole encrypted vault
//!   ([`VaultCipher`]), which carries the managed key bodies, the shared
//!   passwords, their metadata and the accepted server identities, and
//!   replaces the version 1 password block. A version 2 document never holds
//!   both.
//!
//! The workspace, the batch tasks and runs, the audit log and the sync settings
//! are machine state and are never part of the document.
//!
//! Reading a document is strict: a damaged file, a file of another kind, or a file
//! from a newer build is refused before anything is planned. Applying it goes
//! through an [`ApplyPlan`] the user can preview, takes a snapshot of every data
//! file first, and puts back every file it already wrote when a later step fails.

mod apply;
mod crypto;
mod rebase;
mod snapshot;
mod sync_merge;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shellune_core::{
    ConnectionDefaults, ConnectionRecord, ForwardBook, GROUPS_FORMAT_VERSION, GroupBook, HostId,
    SecretString, SnippetBook,
};

use crate::book::{FORMAT_VERSION, HostBook, RecentConnection};
use crate::forwarding::ForwardStore;
use crate::group::GroupStore;
use crate::preferences::{KeymapEntry, Preferences, PreferencesStore};
use crate::snippets::SnippetStore;
use crate::store::{HostStore, data_directory, write_atomic_file};
use crate::trust::{HostTrust, TrustStore};
use crate::vault::{VaultCipher, VaultContent};

pub use apply::{
    ApplyMode, ApplyOutcome, ApplyPlan, Change, PlatformFallback, PlatformField, TrustConflict,
};
pub use crypto::{KdfParams, SecretBundle};
pub use rebase::{Rebased, Touched, rebase_state};
pub use snapshot::{SNAPSHOT_DIRECTORY, SNAPSHOT_LIMIT, Snapshot, SnapshotStore};
pub use sync_merge::{
    ConflictSection, DocumentConflict, DocumentMerge, MergeSide, merge_documents,
};

use crypto::SecretBlock;

/// The value of the `format` field: what tells this document from any other JSON.
pub const PORTABLE_FORMAT: &str = "shellune-data";

/// The document version this build writes when it carries no vault.
///
/// A document of this version is what every earlier build already reads and
/// writes, which is why it stays the default for data without a vault.
pub const PORTABLE_VERSION: u32 = 1;

/// The document version that carries the encrypted vault, and the newest this
/// build reads (ADR-2).
///
/// A version 2 document holds no version 1 password block and no plaintext
/// `known_hosts`: the secrets and the accepted server identities live in the
/// [`VaultCipher`] instead. An older build refuses it by the version check it
/// already has, so an old client cannot downgrade or silently drop the vault.
pub const PORTABLE_VAULT_VERSION: u32 = 2;

/// Keys that mean a credential. The preference and platform parts are read
/// leniently (a newer build may add a choice), so they are searched for these
/// instead of relying on an unknown-field refusal.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passphrase",
    "secret",
    "token",
    "credential",
    "credentials",
    "private_key",
    "privatekey",
    "private-key",
];

/// Where an export reads the remembered host passwords it seals (ADR-6).
///
/// The vault is the source: reading a password from it never touches the
/// system credential store, so an export of hundreds of hosts asks the store
/// for nothing beyond opening the vault once.
pub(crate) trait HostSecretSource {
    /// The remembered password of `host`, when the source holds it.
    fn get(&self, host: HostId) -> Option<SecretString>;
}

/// Where an apply writes the remembered host passwords a document brings and
/// removes the ones nobody reads any more (ADR-6). The changes are in memory
/// until the apply commits them, so a failed apply leaves nothing behind.
pub(crate) trait HostSecrets: HostSecretSource {
    /// Remembers `secret` for `host`; whether anything changed.
    fn set(&mut self, host: HostId, secret: SecretString) -> bool;
    /// Forgets the password of `host` (a tombstone in the vault); whether a
    /// live password was removed.
    fn remove(&mut self, host: HostId) -> bool;
}

impl HostSecretSource for VaultContent {
    fn get(&self, host: HostId) -> Option<SecretString> {
        self.host_secret(host).cloned()
    }
}

/// An unsealed vault being written by one device: the host passwords an apply
/// sets are versioned under `device_id`, like every other vault record.
pub(crate) struct VaultHostSecrets<'a> {
    pub(crate) content: &'a mut VaultContent,
    pub(crate) device_id: &'a str,
}

impl HostSecretSource for VaultHostSecrets<'_> {
    fn get(&self, host: HostId) -> Option<SecretString> {
        self.content.get(host)
    }
}

impl HostSecrets for VaultHostSecrets<'_> {
    fn set(&mut self, host: HostId, secret: SecretString) -> bool {
        self.content.set_host_secret(host, secret, self.device_id)
    }

    fn remove(&mut self, host: HostId) -> bool {
        self.content.remove_host_secret(host)
    }
}

/// Passwords already read, keyed by host: what the compatibility export reads
/// from the credential store before it seals them.
impl HostSecretSource for BTreeMap<HostId, SecretString> {
    fn get(&self, host: HostId) -> Option<SecretString> {
        BTreeMap::get(self, &host).cloned()
    }
}

/// The kind of machine a platform section belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    /// Windows.
    Windows,
    /// macOS.
    Macos,
    /// Linux and the other Unix-like systems.
    Linux,
}

impl Platform {
    /// Every platform, in a fixed order.
    pub const ALL: [Self; 3] = [Self::Windows, Self::Macos, Self::Linux];

    /// The platform this build runs on.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    /// The name used in the document and by [`crate::KeymapPreferences`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }

    /// The platform a document name stands for.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|platform| platform.as_str() == name)
    }
}

/// Why a portable document could not be read, written or applied.
///
/// No variant carries a password: the reasons name fields and files only.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PortableError {
    /// The file could not be read at all.
    #[error("the data file at {path} could not be read: {reason}")]
    Read {
        /// The file that was read.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The file is JSON (or not even that) but not a Shellune data file.
    #[error("the file is not a Shellune data file: {reason}")]
    NotShelluneData {
        /// What gave it away.
        reason: String,
    },
    /// The file claims to be a Shellune data file but is damaged.
    #[error("the data file is damaged: {reason}")]
    Corrupt {
        /// What could not be understood.
        reason: String,
    },
    /// A newer build wrote the file (or one of the documents inside it).
    #[error("the data file uses format {found}, but this build reads up to {supported}")]
    UnsupportedVersion {
        /// The version found.
        found: u32,
        /// The newest this build reads.
        supported: u32,
    },
    /// The passwords could not be opened: the passphrase is wrong or the sealed
    /// bytes were changed. The two cannot be told apart.
    #[error("the passphrase is wrong or the file is damaged")]
    WrongPassphrase,
    /// An empty passphrase was given.
    #[error("the passphrase is empty")]
    EmptyPassphrase,
    /// A data file on this machine could not be read, so nothing can be exported
    /// from it or planned against it.
    #[error("the {label} on this machine could not be read: {reason}")]
    Local {
        /// Which data.
        label: &'static str,
        /// Why.
        reason: String,
    },
    /// The passwords could not be sealed.
    #[error("the saved passwords could not be encrypted: {reason}")]
    Encrypt {
        /// Why.
        reason: String,
    },
    /// The document could not be serialized.
    #[error("the data file could not be encoded: {reason}")]
    Encode {
        /// Why.
        reason: String,
    },
    /// The data file could not be written to the chosen place.
    #[error("the data file could not be written to {path}: {reason}")]
    Write {
        /// The target.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The snapshot taken before applying could not be made; nothing was changed.
    #[error(
        "a copy of the current data could not be made in {path}: {reason}. Nothing was changed"
    )]
    Snapshot {
        /// The snapshot directory.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The data on this machine changed after the plan was made (for example a
    /// save in the background), so applying it would lose that change. Nothing
    /// was written; plan again from the current data.
    #[error("the data on this machine changed after the preview. Nothing was changed")]
    Changed,
    /// Applying stopped part way. Every file already written was put back when
    /// `restored` is true; the snapshot holds the previous files either way.
    #[error("{step} failed: {reason}. {}", restored_sentence(*restored))]
    Apply {
        /// What was being done.
        step: String,
        /// Why it failed.
        reason: String,
        /// Whether everything already written was put back.
        restored: bool,
        /// The snapshot that holds the previous files.
        snapshot: PathBuf,
    },
}

fn restored_sentence(restored: bool) -> &'static str {
    if restored {
        "The previous data was put back"
    } else {
        "The previous data could not be put back completely; a copy is in the snapshot folder"
    }
}

/// The machine-independent part of a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedData {
    hosts: HostBook,
    #[serde(default)]
    groups: GroupBook,
    #[serde(default)]
    forwards: ForwardBook,
    #[serde(default)]
    snippets: SnippetBook,
    #[serde(default)]
    known_hosts: HostTrust,
    #[serde(default)]
    preferences: SharedPreferences,
}

impl SharedData {
    /// The host book: hosts, other connections, recents and favourites.
    #[must_use]
    pub fn hosts(&self) -> &HostBook {
        &self.hosts
    }

    /// The group tree.
    #[must_use]
    pub fn groups(&self) -> &GroupBook {
        &self.groups
    }

    /// The forwarding rules.
    #[must_use]
    pub fn forwards(&self) -> &ForwardBook {
        &self.forwards
    }

    /// The saved commands.
    #[must_use]
    pub fn snippets(&self) -> &SnippetBook {
        &self.snippets
    }

    /// The accepted server identities.
    #[must_use]
    pub fn known_hosts(&self) -> &HostTrust {
        &self.known_hosts
    }

    /// The preferences that do not depend on the machine.
    #[must_use]
    pub fn preferences(&self) -> &SharedPreferences {
        &self.preferences
    }
}

/// The preferences that mean the same on every machine.
///
/// The text size and the sidebar depend on the screen, and the key bindings on the
/// keyboard, so those are in [`PlatformData`] instead.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedPreferences {
    /// `system`, `light` or `dark`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// `en` or `zh`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// `full` or `reduced`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<String>,
    /// Terminal font size in points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_font_size: Option<f32>,
    /// The terminal palette.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_scheme: Option<String>,
    /// Whether a bell in a background tab marks it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bell_notice: Option<bool>,
    /// Whether a finished command in a background tab marks it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_notice: Option<bool>,
    /// The keepalive and connect timeout every host inherits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionDefaults>,
}

impl SharedPreferences {
    /// The shared part of the local preferences.
    #[must_use]
    pub fn from_preferences(preferences: &Preferences) -> Self {
        Self {
            theme: preferences.theme.clone(),
            language: preferences.language.clone(),
            motion: preferences.motion.clone(),
            terminal_font_size: preferences.terminal_font_size,
            terminal_scheme: preferences.terminal_scheme.clone(),
            bell_notice: preferences.bell_notice,
            command_notice: preferences.command_notice,
            connection: preferences.connection,
        }
    }

    /// Writes these choices into local preferences.
    ///
    /// With `only_set`, a choice the document leaves unset keeps the local value
    /// (a merge); without it, the local value is cleared too (a replacement).
    pub fn apply_to(&self, preferences: &mut Preferences, only_set: bool) {
        fn put<T: Clone>(target: &mut Option<T>, value: &Option<T>, only_set: bool) {
            if value.is_some() || !only_set {
                target.clone_from(value);
            }
        }
        put(&mut preferences.theme, &self.theme, only_set);
        put(&mut preferences.language, &self.language, only_set);
        put(&mut preferences.motion, &self.motion, only_set);
        put(
            &mut preferences.terminal_font_size,
            &self.terminal_font_size,
            only_set,
        );
        put(
            &mut preferences.terminal_scheme,
            &self.terminal_scheme,
            only_set,
        );
        put(&mut preferences.bell_notice, &self.bell_notice, only_set);
        put(
            &mut preferences.command_notice,
            &self.command_notice,
            only_set,
        );
        put(&mut preferences.connection, &self.connection, only_set);
    }
}

/// What one platform keeps for itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformData {
    /// The key bindings changed on this platform. `None` says nothing about them;
    /// an empty list says every action uses its default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keymap: Option<Vec<KeymapEntry>>,
    /// Interface text size in percent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_size: Option<u16>,
    /// Whether the sidebar is collapsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_collapsed: Option<bool>,
    /// Per record: the machine-specific fields as this platform has them. A record
    /// listed here with no field set has none of them on this platform.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<HostId, HostOverlay>,
}

/// The fields of one record that differ from machine to machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostOverlay {
    /// The private key file of an SSH host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// The device of a serial connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_device: Option<String>,
    /// The local folder an RDP connection shows the remote desktop as a drive.
    ///
    /// Unlike the other two fields this one is never in the shared record: a folder
    /// chosen on one machine is only ever used on a machine of the same platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_folder: Option<String>,
}

/// One portable data document (see the module documentation).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShelluneData {
    format: String,
    version: u32,
    created_unix: u64,
    source_platform: String,
    source_app_version: String,
    shared: SharedData,
    /// Kept as written so the sections of other platforms — including fields this
    /// build does not know — are carried over exactly.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    platforms: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secrets: Option<SecretBlock>,
    /// The encrypted vault, only of a version 2 document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault: Option<VaultCipher>,
}

/// How a document is exported.
#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// The platform whose section is written.
    pub platform: Platform,
    /// When set, the saved passwords are sealed into the document under this
    /// passphrase; when absent, the document holds no password at all.
    pub passphrase: Option<SecretString>,
    /// The cost of deriving the key from the passphrase.
    pub kdf: KdfParams,
    /// The time written into the document.
    pub created_unix: u64,
    /// The application version written into the document.
    pub app_version: String,
    /// The encrypted vault, which makes the document a version 2 one.
    vault: Option<VaultCipher>,
}

impl ExportOptions {
    /// A document for `platform`, without passwords, stamped now.
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            passphrase: None,
            kdf: KdfParams::default(),
            created_unix: now_unix(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            vault: None,
        }
    }

    /// The same options, sealing the saved passwords under `passphrase`.
    ///
    /// This is the version 1 password block, so it and the vault are mutually
    /// exclusive: taking this clears the vault.
    #[must_use]
    pub fn with_passphrase(mut self, passphrase: SecretString) -> Self {
        self.passphrase = Some(passphrase);
        self.vault = None;
        self
    }

    /// The same options, carrying the encrypted vault as a version 2 document
    /// instead of the version 1 password block and the plaintext trust records
    /// (R9). A version 2 document never holds both secrets sections, so taking
    /// this clears the passphrase.
    #[must_use]
    pub fn with_vault(mut self, vault: VaultCipher) -> Self {
        self.passphrase = None;
        self.vault = Some(vault);
        self
    }

    /// The encrypted vault the document would carry, when it would.
    #[must_use]
    pub fn vault(&self) -> Option<&VaultCipher> {
        self.vault.as_ref()
    }
}

/// What an export produced.
#[derive(Clone, Debug)]
pub struct ExportOutcome {
    /// The document.
    pub document: ShelluneData,
    /// The records whose password was sealed into the document.
    pub secrets_included: Vec<HostId>,
    /// The records that remember a password that could not be read. Host
    /// passwords come from the vault alone, which an export either has open
    /// or not, so this stays empty; it is kept for the reports that list it.
    pub secrets_unreadable: Vec<HostId>,
    /// The records that remember a password the vault does not hold (no vault,
    /// or a host whose old system-store entry was not migrated yet).
    pub secrets_missing: Vec<HostId>,
}

impl ShelluneData {
    /// Builds a document from the local data.
    ///
    /// `base`, when given, is the document this one replaces (the copy in the
    /// cloud): the sections of the other platforms are carried over from it
    /// unchanged. Its sealed passwords are not: a new block is sealed when
    /// `options.passphrase` is set, and none is written otherwise. Its vault is
    /// carried over the same way — only what `options.vault` names travels.
    ///
    /// The passwords a version 1 block seals come from `vault` alone (R4): a
    /// record that remembers a password the vault does not hold — no vault, or
    /// a host not migrated yet — is listed in
    /// [`ExportOutcome::secrets_missing`], and the system credential store is
    /// never asked for it.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::EmptyPassphrase`] for a blank passphrase and
    /// [`PortableError::Encrypt`] when the passwords could not be sealed.
    pub fn export_from_vault(
        local: &LocalState,
        options: &ExportOptions,
        vault: Option<&VaultContent>,
        base: Option<&ShelluneData>,
    ) -> Result<ExportOutcome, PortableError> {
        let empty = BTreeMap::new();
        let source: &dyn HostSecretSource = match vault {
            Some(content) => content,
            None => &empty,
        };
        Self::export_with(local, options, source, base)
    }

    fn export_with(
        local: &LocalState,
        options: &ExportOptions,
        secrets: &dyn HostSecretSource,
        base: Option<&ShelluneData>,
    ) -> Result<ExportOutcome, PortableError> {
        let platform = options.platform;
        // A vault-carrying document is a version 2 one: it writes no version 1
        // password block and no plaintext trust records, and an old build
        // refuses it by the version check instead of dropping them.
        let carries_vault = options.vault.is_some();
        let section = PlatformData::from_local(local, platform);
        let mut platforms = base.map(|base| base.platforms.clone()).unwrap_or_default();
        platforms.insert(
            platform.as_str().to_owned(),
            serde_json::to_value(&section).map_err(|error| PortableError::Encode {
                reason: error.to_string(),
            })?,
        );

        let mut outcome = ExportOutcome {
            document: Self {
                format: PORTABLE_FORMAT.to_owned(),
                version: if carries_vault {
                    PORTABLE_VAULT_VERSION
                } else {
                    PORTABLE_VERSION
                },
                created_unix: options.created_unix,
                source_platform: platform.as_str().to_owned(),
                source_app_version: options.app_version.clone(),
                shared: SharedData {
                    // Which passwords this machine's credential store holds is
                    // not shared; the sealed block says which ones travel. A
                    // mapped folder is not shared either: it is in this
                    // platform's section above.
                    hosts: apply::shared_book(&local.hosts)?,
                    groups: local.groups.clone(),
                    forwards: local.forwards.clone(),
                    snippets: local.snippets.clone(),
                    // From version 2 on the accepted identities are the
                    // vault's to guard, so the shared plaintext part stops
                    // carrying a second authority (ADR-6).
                    known_hosts: if carries_vault {
                        HostTrust::new()
                    } else {
                        local.known_hosts.clone()
                    },
                    preferences: SharedPreferences::from_preferences(&local.preferences),
                },
                platforms,
                secrets: None,
                vault: options.vault.clone(),
            },
            secrets_included: Vec::new(),
            secrets_unreadable: Vec::new(),
            secrets_missing: Vec::new(),
        };

        if let Some(passphrase) = &options.passphrase {
            if carries_vault {
                // The builders keep the two apart; the fields are public, so
                // the refusal is spelled out for a caller that sets both.
                return Err(PortableError::Encrypt {
                    reason: "a version 2 document carries the vault; the version 1 password block cannot travel with it"
                        .to_owned(),
                });
            }
            if passphrase.expose().trim().is_empty() {
                return Err(PortableError::EmptyPassphrase);
            }
            let mut sealed = BTreeMap::new();
            for record in local.hosts.records() {
                if !record.remembers_secret() {
                    continue;
                }
                match secrets.get(record.id()) {
                    Some(secret) => {
                        sealed.insert(record.id(), secret);
                        outcome.secrets_included.push(record.id());
                    }
                    None => outcome.secrets_missing.push(record.id()),
                }
            }
            outcome.document.secrets = Some(SecretBlock::seal(&sealed, passphrase, options.kdf)?);
        }
        Ok(outcome)
    }

    /// Reads a document from JSON, refusing anything this build cannot apply.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::NotShelluneData`] for other JSON or other bytes,
    /// [`PortableError::UnsupportedVersion`] for a newer document (checked before
    /// the rest is interpreted), and [`PortableError::Corrupt`] for a document
    /// that breaks a rule any of the stores would enforce.
    pub fn from_json(bytes: &[u8]) -> Result<Self, PortableError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| PortableError::NotShelluneData {
                reason: error.to_string(),
            })?;
        let Some(object) = value.as_object() else {
            return Err(PortableError::NotShelluneData {
                reason: "the file is not a JSON object".to_owned(),
            });
        };
        if object.get("format").and_then(serde_json::Value::as_str) != Some(PORTABLE_FORMAT) {
            return Err(PortableError::NotShelluneData {
                reason: format!("the \"format\" field is not \"{PORTABLE_FORMAT}\""),
            });
        }
        let version = object
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| PortableError::Corrupt {
                reason: "the format version is missing or not a whole number".to_owned(),
            })?;
        if version > PORTABLE_VAULT_VERSION {
            return Err(PortableError::UnsupportedVersion {
                found: version,
                supported: PORTABLE_VAULT_VERSION,
            });
        }
        if version == 0 {
            return Err(PortableError::Corrupt {
                reason: "format version 0 was never written by Shellune".to_owned(),
            });
        }
        // The two secrets sections belong to their own versions: a document
        // claiming both is neither format, and reading it would guess which
        // secrets to trust.
        if version >= PORTABLE_VAULT_VERSION && object.get("secrets").is_some() {
            return Err(PortableError::Corrupt {
                reason: format!(
                    "a version {version} document cannot carry the version 1 saved passwords"
                ),
            });
        }
        if version < PORTABLE_VAULT_VERSION && object.get("vault").is_some() {
            return Err(PortableError::Corrupt {
                reason: format!(
                    "the encrypted vault only travels with format {PORTABLE_VAULT_VERSION}, not with {version}"
                ),
            });
        }
        // The inner documents carry their own versions; a newer one is named as
        // such rather than reported as an unknown field.
        if let Some(shared) = object.get("shared").and_then(serde_json::Value::as_object) {
            for (key, supported) in [
                ("hosts", FORMAT_VERSION),
                ("groups", GROUPS_FORMAT_VERSION),
                ("forwards", shellune_core::FORWARD_FORMAT_VERSION),
                ("snippets", shellune_core::SNIPPETS_FORMAT_VERSION),
                ("known_hosts", crate::trust::TRUST_FORMAT_VERSION),
            ] {
                let found = shared
                    .get(key)
                    .and_then(|document| document.get("version"))
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|version| u32::try_from(version).ok());
                if let Some(found) = found
                    && found > supported
                {
                    return Err(PortableError::UnsupportedVersion { found, supported });
                }
            }
        }
        // The preferences and the platform sections are read leniently, so a field
        // that names a credential would be dropped silently by the typed reader.
        // It is refused here instead, by name and without its value.
        let lenient = [
            (
                "the preferences".to_owned(),
                object
                    .get("shared")
                    .and_then(|shared| shared.get("preferences")),
            ),
            ("the platform sections".to_owned(), object.get("platforms")),
        ];
        for (label, part) in lenient {
            if let Some(key) = part.and_then(find_secret_key) {
                return Err(PortableError::Corrupt {
                    reason: format!(
                        "{label} contain the field \"{key}\", which cannot be stored. Its value was not kept"
                    ),
                });
            }
        }

        let document: Self =
            serde_json::from_value(value).map_err(|error| PortableError::Corrupt {
                reason: error.to_string(),
            })?;
        document
            .validate()
            .map_err(|reason| PortableError::Corrupt { reason })?;
        Ok(document)
    }

    /// Reads a document from a file.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Read`] when the file cannot be read, and every
    /// error of [`Self::from_json`].
    pub fn read(path: &Path) -> Result<Self, PortableError> {
        let bytes = std::fs::read(path).map_err(|error| PortableError::Read {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
        Self::from_json(&bytes)
    }

    /// The document as indented JSON.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Encode`] when it cannot be serialized.
    pub fn to_json(&self) -> Result<Vec<u8>, PortableError> {
        serde_json::to_vec_pretty(self).map_err(|error| PortableError::Encode {
            reason: error.to_string(),
        })
    }

    /// Writes the document to `path` without leaving a partial file behind.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Write`] when the file could not be written; an
    /// earlier file at that path is left as it was.
    pub fn write_to(&self, path: &Path) -> Result<(), PortableError> {
        let bytes = self.to_json()?;
        let write_error = |reason: String| PortableError::Write {
            path: path.to_owned(),
            reason,
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| write_error("the path does not name a file".to_owned()))?;
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        write_atomic_file(parent, name, &bytes).map_err(|error| write_error(error.to_string()))
    }

    /// The format version of the document.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// When the document was made, in seconds since the Unix epoch.
    #[must_use]
    pub fn created_unix(&self) -> u64 {
        self.created_unix
    }

    /// The platform the document was made on, as written (`windows`, `macos`,
    /// `linux`, or a name a newer build uses).
    #[must_use]
    pub fn source_platform(&self) -> &str {
        &self.source_platform
    }

    /// The application version that made the document.
    #[must_use]
    pub fn source_app_version(&self) -> &str {
        &self.source_app_version
    }

    /// The machine-independent records.
    #[must_use]
    pub fn shared(&self) -> &SharedData {
        &self.shared
    }

    /// The section one platform wrote, when there is one.
    #[must_use]
    pub fn platform(&self, platform: Platform) -> Option<PlatformData> {
        self.platforms
            .get(platform.as_str())
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// The names of the platform sections the document holds.
    pub fn platform_names(&self) -> impl Iterator<Item = &str> {
        self.platforms.keys().map(String::as_str)
    }

    /// Whether the document carries sealed passwords.
    #[must_use]
    pub fn has_secrets(&self) -> bool {
        self.secrets.is_some()
    }

    /// Whether the document carries the encrypted vault.
    #[must_use]
    pub fn has_vault(&self) -> bool {
        self.vault.is_some()
    }

    /// The encrypted vault the document carries, when it carries one.
    #[must_use]
    pub fn vault(&self) -> Option<&VaultCipher> {
        self.vault.as_ref()
    }

    /// The key-derivation cost of the sealed passwords, when there are any.
    #[must_use]
    pub fn secrets_kdf(&self) -> Option<KdfParams> {
        self.secrets.as_ref().map(SecretBlock::params)
    }

    /// Opens the sealed passwords. A document without any opens to an empty
    /// bundle.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::WrongPassphrase`] for a wrong passphrase or changed
    /// bytes, and [`PortableError::EmptyPassphrase`] for a blank one.
    pub fn open_secrets(&self, passphrase: &SecretString) -> Result<SecretBundle, PortableError> {
        match &self.secrets {
            Some(block) => block.open(passphrase),
            None => Ok(SecretBundle::default()),
        }
    }

    /// The document as one platform sees it, in a canonical byte form: the shared
    /// part and that platform's section, with object keys in sorted order and no
    /// time stamp or source. Two documents with equal views hold the same data for
    /// that machine, which is what sync compares.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Encode`] when the view cannot be serialized.
    pub fn sync_view(&self, platform: Platform) -> Result<Vec<u8>, PortableError> {
        self.view(platform, true)
    }

    /// The same view as [`Self::sync_view`] without the recent-connection log:
    /// what the document holds apart from when each host was last reached. Sync
    /// compares it to tell a side that only connected from one that changed
    /// records.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Encode`] when the view cannot be serialized.
    pub fn core_view(&self, platform: Platform) -> Result<Vec<u8>, PortableError> {
        self.view(platform, false)
    }

    /// The same document with its recent-connection log replaced by `recents`.
    /// An entry naming a record the document does not hold is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Corrupt`] when the host book cannot be rebuilt,
    /// which a document that passed [`Self::from_json`] does not cause.
    pub fn with_recents(&self, recents: &[RecentConnection]) -> Result<Self, PortableError> {
        let mut document = self.clone();
        document.shared.hosts = apply::build_book(
            &self.shared.hosts.records(),
            recents,
            self.shared.hosts.favorites(),
            self.shared.hosts.unreadable_connections(),
        )?;
        Ok(document)
    }

    /// Marks the document as made now by this build on `options.platform` (the
    /// time and version `options` carry); nothing else changes.
    pub fn restamp(&mut self, options: &ExportOptions) {
        self.created_unix = options.created_unix;
        self.source_platform = options.platform.as_str().to_owned();
        self.source_app_version.clone_from(&options.app_version);
    }

    fn view(&self, platform: Platform, with_recents: bool) -> Result<Vec<u8>, PortableError> {
        let encode = |error: serde_json::Error| PortableError::Encode {
            reason: error.to_string(),
        };
        let mut shared = serde_json::to_value(&self.shared).map_err(encode)?;
        if !with_recents
            && let Some(hosts) = shared
                .get_mut("hosts")
                .and_then(serde_json::Value::as_object_mut)
        {
            hosts.remove("recents");
        }
        let section = self
            .platforms
            .get(platform.as_str())
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let view = serde_json::json!({ "shared": shared, "platform": section });
        serde_json::to_vec(&canonical(view)).map_err(encode)
    }

    /// Plans applying this document to the local data. Nothing is written.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Corrupt`] when the combined records would break a
    /// rule of the stores.
    pub fn plan(
        &self,
        local: &LocalState,
        platform: Platform,
        mode: ApplyMode,
        secrets: Option<&SecretBundle>,
    ) -> Result<ApplyPlan, PortableError> {
        ApplyPlan::new(self, local, platform, mode, secrets)
    }

    fn validate(&self) -> Result<(), String> {
        if self.source_platform.trim().is_empty() {
            return Err("the document does not say which platform made it".to_owned());
        }
        let shared = &self.shared;
        if shared.hosts.version() == 0 {
            return Err("the hosts use format version 0, which Shellune never wrote".to_owned());
        }
        shared.hosts.ensure_valid()?;
        if shared.groups.version() == 0 || shared.groups.version() > GROUPS_FORMAT_VERSION {
            return Err(format!(
                "the groups use format version {}, which this build does not read",
                shared.groups.version()
            ));
        }
        shared.groups.validate()?;
        shared.forwards.validate()?;
        shared.snippets.validate()?;
        shared.known_hosts.validate()?;
        for (name, section) in &self.platforms {
            if !section.is_object() {
                return Err(format!(
                    "the section of the platform {name} is not an object"
                ));
            }
            if let Some(key) = find_secret_key(section) {
                return Err(format!(
                    "the section of the platform {name} contains the field \"{key}\", which cannot be stored"
                ));
            }
            if Platform::parse(name).is_some() {
                let data: PlatformData = serde_json::from_value(section.clone())
                    .map_err(|error| format!("the section of the platform {name}: {error}"))?;
                for (id, overlay) in &data.hosts {
                    for value in [
                        &overlay.identity_file,
                        &overlay.serial_device,
                        &overlay.drive_folder,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if value.trim().is_empty() || value.chars().any(char::is_control) {
                            return Err(format!(
                                "the section of the platform {name} holds an unusable path for the record {id}"
                            ));
                        }
                    }
                }
            }
        }
        if let Some(block) = &self.secrets {
            block.validate()?;
        }
        // The reader already refuses the mix in a file; a document built in
        // memory is held to the same rule before it can be written.
        if self.version >= PORTABLE_VAULT_VERSION && self.secrets.is_some() {
            return Err(format!(
                "a version {} document cannot carry the version 1 saved passwords",
                self.version
            ));
        }
        if self.version < PORTABLE_VAULT_VERSION && self.vault.is_some() {
            return Err(format!(
                "the encrypted vault only travels with format {PORTABLE_VAULT_VERSION}"
            ));
        }
        if let Some(cipher) = &self.vault {
            cipher
                .validate()
                .map_err(|reason| format!("the encrypted vault is not valid: {reason}"))?;
        }
        Ok(())
    }
}

impl PlatformData {
    /// The section the local data would write for `platform`.
    #[must_use]
    pub fn from_local(local: &LocalState, platform: Platform) -> Self {
        let preferences = &local.preferences;
        let keymap = preferences
            .keymap
            .as_ref()
            .map(|keymap| keymap.for_platform(platform.as_str()).to_vec())
            .unwrap_or_default();
        let mut hosts = BTreeMap::new();
        for record in local.hosts.records() {
            let overlay = match &record {
                ConnectionRecord::Ssh(host) => HostOverlay {
                    identity_file: host.identity_file().map(str::to_owned),
                    ..HostOverlay::default()
                },
                ConnectionRecord::Serial(serial) => HostOverlay {
                    serial_device: Some(serial.device().to_owned()),
                    ..HostOverlay::default()
                },
                // Listed even without a folder: an entry with none says this
                // platform maps nothing, so clearing the folder travels too.
                ConnectionRecord::Rdp(rdp) => HostOverlay {
                    drive_folder: rdp.drive_folder().map(str::to_owned),
                    ..HostOverlay::default()
                },
                ConnectionRecord::Vnc(_) => continue,
            };
            hosts.insert(record.id(), overlay);
        }
        Self {
            keymap: Some(keymap),
            text_size: preferences.text_size,
            sidebar_collapsed: preferences.sidebar_collapsed,
            hosts,
        }
    }
}

/// The data directory a document is exported from and applied to.
#[derive(Clone, Debug)]
pub struct LocalData {
    root: PathBuf,
}

impl LocalData {
    /// The data files inside `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The per-user data directory of this platform.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(data_directory())
    }

    /// The data directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The snapshots kept in this data directory.
    #[must_use]
    pub fn snapshots(&self) -> SnapshotStore {
        SnapshotStore::at(&self.root)
    }

    /// Reads every data file a document covers.
    ///
    /// Missing files are empty data. A damaged preferences file reads as the
    /// defaults, the way the application itself reads it.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Local`] naming the first file that is damaged,
    /// unreadable or from a newer build.
    pub fn load(&self) -> Result<LocalState, PortableError> {
        fn local(label: &'static str, error: &dyn std::fmt::Display) -> PortableError {
            PortableError::Local {
                label,
                reason: error.to_string(),
            }
        }
        Ok(LocalState {
            hosts: HostStore::at(&self.root)
                .load()
                .map_err(|error| local("saved hosts", &error))?,
            groups: GroupStore::at(&self.root)
                .load()
                .map_err(|error| local("host groups", &error))?,
            forwards: ForwardStore::at(&self.root)
                .load()
                .map_err(|error| local("forwarding rules", &error))?,
            snippets: SnippetStore::at(&self.root)
                .load()
                .map_err(|error| local("saved commands", &error))?,
            known_hosts: TrustStore::at(&self.root)
                .load()
                .map_err(|error| local("trusted server identities", &error))?,
            preferences: PreferencesStore::at(&self.root).load(),
        })
    }
}

/// The local data a document is made from or applied to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalState {
    /// `hosts.json`.
    pub hosts: HostBook,
    /// `groups.json`.
    pub groups: GroupBook,
    /// `forwards.json`.
    pub forwards: ForwardBook,
    /// `snippets.json`.
    pub snippets: SnippetBook,
    /// `known_hosts.json`.
    pub known_hosts: HostTrust,
    /// `preferences.json`.
    pub preferences: Preferences,
}

impl LocalState {
    /// Whether no record of any kind is saved. Preferences do not count: a
    /// machine with only a chosen theme has nothing worth keeping from a pull.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hosts.connection_count() == 0
            && self.groups.is_empty()
            && self.forwards.rules().is_empty()
            && self.snippets.is_empty()
            && self.known_hosts.is_empty()
    }
}

/// Two recent-connection logs as one: one entry per host, the later connection
/// winning and, at the same time, the higher count. Newest first; entries of the
/// same time are ordered by host so both machines end with the same list.
///
/// Entries are not checked against any host book; [`ShelluneData::with_recents`]
/// and [`ApplyPlan::with_recents`] drop the ones whose host is gone.
#[must_use]
pub fn merge_recent_connections(
    left: &[RecentConnection],
    right: &[RecentConnection],
) -> Vec<RecentConnection> {
    let mut newest: BTreeMap<HostId, RecentConnection> = BTreeMap::new();
    for recent in left.iter().chain(right) {
        let newer = |kept: &RecentConnection| {
            (recent.last_connected_unix(), recent.connections())
                > (kept.last_connected_unix(), kept.connections())
        };
        match newest.get(&recent.host()) {
            Some(kept) if !newer(kept) => {}
            _ => {
                newest.insert(recent.host(), *recent);
            }
        }
    }
    let mut merged: Vec<RecentConnection> = newest.into_values().collect();
    merged.sort_by(|a, b| {
        b.last_connected_unix()
            .cmp(&a.last_connected_unix())
            .then_with(|| a.host().cmp(&b.host()))
    });
    merged
}

/// Seconds since the Unix epoch.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

pub(crate) fn find_secret_key(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, child)| {
            if SECRET_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                Some(key.clone())
            } else {
                find_secret_key(child)
            }
        }),
        serde_json::Value::Array(items) => items.iter().find_map(find_secret_key),
        _ => None,
    }
}

/// The same value with every object's keys in sorted order.
fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .into_iter()
                .map(|(key, child)| (key, canonical(child)))
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonical).collect())
        }
        other => other,
    }
}
