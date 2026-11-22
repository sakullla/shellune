//! One sync: compare both sides with what they held last time, then upload, pull,
//! do nothing, or stop at a conflict for the user to decide.
//!
//! What is compared is the portable document as this platform sees it
//! ([`ShelluneData::sync_view`]): the shared records and this platform's section,
//! without time stamps, other platforms or sealed passwords. The local side
//! changed when its view differs from the one recorded after the last successful
//! sync; the remote side changed when the file's entity tag differs and its view
//! differs too (another platform writing only its own section is not a change
//! here).
//!
//! | local \ remote | unchanged       | changed  |
//! |----------------|-----------------|----------|
//! | unchanged      | nothing to do   | pull     |
//! | changed        | upload (If-Match) | conflict or merge |
//!
//! When both sides changed but at least one of them only connected to hosts (its
//! data apart from the recent-connection log, [`ShelluneData::core_view`], is
//! what was last synced — passwords included), automatic sync merges instead of
//! stopping: the records come from the side that changed them (the cloud's when
//! neither did), the recent connections keep the later entry per host, and the
//! result is uploaded with If-Match, then written here (after a snapshot). A
//! refused upload or a save made here meanwhile decides again. Both sides
//! changing records is still a conflict, and "upload now" and "pull" never merge.
//!
//! A first sync with a server (no state yet): no file or an empty one in the cloud
//! uploads, an empty machine pulls, equal data is only recorded, and anything else
//! is a conflict. A write the server refuses because the file changed meanwhile
//! (412) and an apply refused because local data was saved meanwhile both start
//! the decision over.
//!
//! A pull applies the cloud document as a replacement: when only the cloud
//! changed, this machine holds exactly what was last synced, so the cloud's
//! version is the newer one in full — deletions included. Applying snapshots the
//! local data first (see [`shellune_storage::SnapshotStore`]).
//!
//! # The encrypted vault (R5, R6, R8)
//!
//! When the user asks for the vault to travel, the document becomes a version 2
//! one that carries the encrypted vault ([`shellune_storage::VaultCipher`])
//! instead of the version 1 sealed passwords. The unseal secret is the non-empty
//! WebDAV password when there is one, otherwise the sync passphrase a
//! passwordless account must have first. Reusing the WebDAV password is the
//! user's confirmed choice: the server sees it in every Basic authorization
//! header, so it can decrypt and forge valid ciphertext — that mode is not
//! zero-knowledge towards the WebDAV server, and nothing here may claim it is.
//!
//! The vault has its own decision beside the document's: both sides are opened
//! and merged record by record and trust entry by trust entry against the
//! version baseline of the last successful sync (ADR-5). Different records take
//! changes from both sides; the same record changed differently, or deleted on
//! one side against changed on the other, and one server trusted with two
//! different identities are conflicts the user settles one by one
//! ([`SyncService::resolve_vault`]) — never a last-write-wins, and never the
//! cloud's identity adopted silently. While any conflict stands, no part of the
//! vault is applied; once settled, the merged vault is sealed once, uploaded and
//! applied as one, and a failure keeps the last usable state on both sides. The
//! write stamps the vault's global version — both sides' maximum plus one — as
//! the merging device's, which is what lets a machine with a baseline refuse an
//! abnormally rolled-back cloud instead of applying it. A machine adopting a
//! vault without a baseline is told the version it landed on and claims nothing
//! about recognising rollbacks of the past.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use shellune_core::{
    CredentialId, CredentialMeta, HostConfig, HostId, KeyId, ManagedKeyMeta, SecretString,
    VaultRecordVersion,
};
use shellune_storage::{
    AppSecret, ApplyMode, ApplyOutcome, ApplyPlan, Change, ExportOptions, HostReference, KdfParams,
    LocalData, LocalState, PassphrasePurpose, Platform, PlatformFallback, PortableError,
    SecretBundle, SecretError, SecretStore, ShelluneData, SnapshotStore, TrustConflict,
    UnsealSource, VaultCipher, VaultContent, VaultCredentialRecord, VaultKeyRecord, VaultStore,
    VaultStoreError, merge_recent_connections,
};

use crate::config::{
    ConfigError, LastResult, SyncConfig, SyncConfigStore, SyncState, SyncStateStore, VaultBaseline,
    VaultRecordBase,
};
use crate::webdav::{Precondition, REMOTE_FILE_NAME, WebDavClient, WebDavError, is_strong_etag};

/// How many times one sync starts over after the cloud or the local data changed
/// under it.
const ATTEMPTS: usize = 3;

/// Why a sync did not complete. Local data is never lost: a failure either
/// happened before anything was written or was rolled back.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    /// No server is set; nothing was sent.
    #[error("sync is not set up")]
    NotConfigured,
    /// The sync settings or state could not be read or written.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The WebDAV password or the sync passphrase could not be read from the
    /// credential store.
    #[error("the sync credentials could not be read: {0}")]
    Credentials(SecretError),
    /// The vault or the saved passwords are to travel, the WebDAV account has no
    /// password, and no non-empty sync passphrase is saved on this machine
    /// (R5: a passwordless account must have one before anything secret syncs).
    #[error(
        "syncing the vault or saved passwords is on, the WebDAV account has no password, \
         and no sync passphrase is set"
    )]
    PassphraseMissing,
    /// The encrypted vault could not be opened, sealed or written. Nothing was
    /// changed: a vault this build cannot use is never overwritten.
    #[error(transparent)]
    Vault(#[from] VaultStoreError),
    /// The cloud holds an older vault than this machine has already applied. An
    /// abnormal rollback (ADR-5): automatic sync stops, and nothing is applied
    /// or uploaded until the user decides.
    #[error(
        "the cloud holds an older vault (version {found}) than this machine already applied (version {applied}); \
         nothing was changed. This can be an abnormal rollback of the cloud file"
    )]
    Rollback { found: u64, applied: u64 },
    /// The server failed or refused.
    #[error(transparent)]
    Remote(#[from] WebDavError),
    /// The file in the cloud cannot be used: damaged, from a newer build, or
    /// sealed under another passphrase. Nothing was changed here.
    #[error("the data file in the cloud cannot be used: {0}")]
    RemoteDocument(PortableError),
    /// A pull was asked for, but the cloud holds no data file.
    #[error("there is no Shellune data in the cloud yet")]
    NothingInCloud,
    /// The local data could not be read, exported or applied.
    #[error(transparent)]
    Local(PortableError),
    /// The cloud or the local data kept changing during the sync.
    #[error("the data kept changing during the sync; it will be tried again")]
    Busy,
}

/// The user's answer to a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Replace this machine's data with the cloud's (a snapshot is kept here).
    UseRemote,
    /// Replace the cloud's data with this machine's (the cloud file is kept in the
    /// snapshot folder here).
    UseLocal,
    /// Change nothing; the conflict stays until it is resolved.
    Cancel,
}

/// How a sync ended.
#[derive(Clone, Debug)]
pub enum SyncOutcome {
    /// Both sides already hold the same data.
    UpToDate,
    /// This machine's data was written to the cloud.
    Uploaded(UploadReport),
    /// The cloud's data was applied here.
    Pulled(Box<PullReport>),
    /// Both sides changed, but at least one only connected to hosts: the two were
    /// merged, uploaded (unless the cloud already held the result) and written
    /// here. The report says what changed here.
    Merged(Box<PullReport>),
    /// Both sides changed; nothing was written. Ask the user and call
    /// [`SyncService::resolve`].
    Conflict(ConflictReport),
    /// The conflict was left for later ([`Resolution::Cancel`]).
    ConflictKept,
}

impl SyncOutcome {
    fn last_result(&self) -> LastResult {
        match self {
            // After a merge both sides hold the same data.
            Self::UpToDate | Self::Merged(_) => LastResult::UpToDate,
            Self::Uploaded(_) => LastResult::Uploaded,
            Self::Pulled(_) => LastResult::Pulled,
            Self::Conflict(_) | Self::ConflictKept => LastResult::Conflict,
        }
    }
}

/// What an upload wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UploadReport {
    /// Whether the file did not exist in the cloud before.
    pub created: bool,
    /// The records whose password was sealed into the file.
    pub secrets_included: Vec<HostId>,
    /// The records that remember a password the credential store could not read.
    pub secrets_unreadable: Vec<HostId>,
    /// The records that remember a password the credential store does not have.
    pub secrets_missing: Vec<HostId>,
    /// Where the cloud file that was replaced was kept, for "use local".
    pub remote_snapshot: Option<PathBuf>,
}

/// What a pull applied.
#[derive(Clone, Debug)]
pub struct PullReport {
    /// The snapshot of the local data taken before; `None` when nothing changed.
    pub snapshot: Option<PathBuf>,
    /// The platform that wrote the cloud file.
    pub source_platform: String,
    /// The application version that wrote it.
    pub source_app_version: String,
    /// When it was written.
    pub created_unix: u64,
    /// Saved connections added, changed, removed.
    pub hosts: Change,
    /// Groups.
    pub groups: Change,
    /// Forwarding rules.
    pub forwards: Change,
    /// Saved commands.
    pub snippets: Change,
    /// Accepted server identities.
    pub known_hosts: Change,
    /// Whether any preference changed.
    pub preferences_changed: bool,
    /// Whether this platform's key bindings changed.
    pub keymap_changed: bool,
    /// Records whose private-key path or serial device came from another machine
    /// (check whether the file exists here).
    pub fallbacks: Vec<PlatformFallback>,
    /// Servers trusted on both sides with different identities; the cloud's now
    /// applies.
    pub trust_conflicts: Vec<TrustConflict>,
    /// Whether the cloud file carried passwords that were not opened because
    /// syncing passwords is off on this machine.
    pub secrets_ignored: bool,
    /// How many passwords were saved to the credential store.
    pub secrets_saved: usize,
    /// Records whose saved password was planned to be removed.
    pub secrets_to_forget: Vec<HostId>,
    /// How many passwords were removed.
    pub secrets_forgotten: usize,
    /// Records whose password could not be removed (an unused entry is left).
    pub secrets_not_forgotten: Vec<HostId>,
    /// The cloud's encrypted vault as applied here, when one travelled (R5, R6):
    /// the version the machine landed on, shown for a first adoption too, which
    /// cannot by itself claim to recognise every rollback.
    pub vault_restored: Option<VaultRestored>,
    /// The local data as it now is, for the interface to reload.
    pub state: LocalState,
}

/// What the user needs to decide a conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictReport {
    /// Whether this is the first sync with this server (both sides already held
    /// different data), as opposed to both changing since the last sync.
    pub first_sync: bool,
    /// The platform that wrote the cloud file.
    pub remote_platform: String,
    /// The application version that wrote it.
    pub remote_app_version: String,
    /// When it was written.
    pub remote_created_unix: u64,
    /// The vault records and trust entries both sides changed differently
    /// (ADR-5, ADR-8). Each one is decided on its own through
    /// [`SyncService::resolve_vault`]; until every one of them is decided,
    /// nothing of the vault is applied. Empty for document-only conflicts and
    /// for whole-document answers ([`Resolution`]).
    pub vault: Vec<VaultConflict>,
}

/// Which kind of vault record a conflict or a choice is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaultRecordKind {
    /// A managed key, private material included.
    ManagedKey,
    /// A shared password.
    Credential,
}

/// One side of a per-record vault conflict, as the interface shows it: public
/// metadata only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultRecordSide {
    /// The name the record goes by.
    pub name: String,
    /// Whether this side deleted the record.
    pub deleted: bool,
    /// How many changes the record had on this side.
    pub counter: u64,
    /// Which device wrote that version.
    pub device_id: String,
}

/// One accepted server identity, as a trust conflict shows it (R8): the
/// algorithm and the complete fingerprint, never a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustIdentity {
    /// The key algorithm as the server named it.
    pub algorithm: String,
    /// The complete SHA-256 fingerprint.
    pub fingerprint: String,
    /// When this identity was accepted, in seconds since the Unix epoch.
    pub accepted_unix: u64,
}

/// One vault conflict the user decides individually (ADR-5, ADR-8). Never
/// resolved automatically, and never by "the cloud is newer".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultConflict {
    /// Both sides changed one record differently — two different edits, or an
    /// edit on one side against a deletion on the other ([`VaultRecordSide::deleted`]).
    Record {
        /// Which kind of record.
        kind: VaultRecordKind,
        /// The stable identifier, as text.
        id: String,
        /// The name, for the preview.
        name: String,
        /// What this machine holds.
        local: VaultRecordSide,
        /// What the cloud holds.
        remote: VaultRecordSide,
    },
    /// Both sides trust a different identity for one server (ADR-8): the full
    /// old and new fingerprints, where the remote one came from, and which side
    /// deleted the trust (an identity of `None`).
    Trust {
        /// The server name or address.
        host: String,
        /// The server port.
        port: u16,
        /// What this machine trusts, when it trusts one.
        local: Option<TrustIdentity>,
        /// What the cloud trusts, when it trusts one.
        remote: Option<TrustIdentity>,
        /// The platform that wrote the cloud's vault.
        remote_source_platform: String,
        /// The application version that wrote it.
        remote_app_version: String,
        /// When the cloud document was written.
        remote_created_unix: u64,
    },
}

/// Which conflict a choice answers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum VaultChoiceKey {
    /// One record conflict.
    Record {
        /// Which kind of record.
        kind: VaultRecordKind,
        /// The stable identifier, as text.
        id: String,
    },
    /// One trust conflict.
    Trust {
        /// The server name or address.
        host: String,
        /// The server port.
        port: u16,
    },
}

/// Which side of a conflict the user kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultChoiceSide {
    /// This machine's version.
    Local,
    /// The cloud's version.
    Remote,
}

/// The user's answer to one vault conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultChoice {
    /// Which conflict this answers.
    pub key: VaultChoiceKey,
    /// Which side was kept.
    pub side: VaultChoiceSide,
}

/// What a sync that applied the cloud's vault restored, so the interface can
/// show the version it landed on (R6). A device adopting a vault without a
/// baseline of its own shows this and claims nothing about detecting rollbacks
/// of the past.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultRestored {
    /// The global version counter of the vault as applied.
    pub global_counter: u64,
    /// How many live records it holds.
    pub live_records: usize,
    /// Whether this machine had no vault baseline of its own (a first adoption,
    /// which cannot by itself tell an old cloud file from a new one).
    pub first_sync: bool,
}

/// What the direction of a sync allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    /// Whatever the comparison says.
    Auto,
    /// "Upload now": never pulls; a newer cloud file is a conflict.
    Upload,
    /// "Pull from the cloud": never uploads; unsynced local changes are a conflict.
    Pull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Nothing,
    Upload,
    Pull,
    Merge,
    Conflict,
}

/// Whether each side changed more than its recent connections since the last
/// sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoreChanges {
    local: bool,
    remote: bool,
}

/// The facts one decision is made from.
#[derive(Clone, Copy, Debug)]
struct Facts {
    first_sync: bool,
    local_changed: bool,
    remote_changed: bool,
    local_empty: bool,
    remote_empty: bool,
    same_view: bool,
    /// Known only when both sides changed, the last sync recorded the data
    /// without recents, and (with passwords synced) the cloud's passwords opened.
    core: Option<CoreChanges>,
}

fn decide(direction: Direction, facts: Facts) -> Action {
    if facts.same_view {
        return Action::Nothing;
    }
    if facts.first_sync {
        return match direction {
            Direction::Auto if facts.local_empty => Action::Pull,
            Direction::Auto | Direction::Upload if facts.remote_empty => Action::Upload,
            Direction::Pull if facts.local_empty => Action::Pull,
            _ => Action::Conflict,
        };
    }
    match (facts.local_changed, facts.remote_changed, direction) {
        (false, false, _) => Action::Nothing,
        (true, false, Direction::Auto | Direction::Upload) => Action::Upload,
        (false, true, Direction::Auto | Direction::Pull) => Action::Pull,
        (true, true, Direction::Auto)
            if facts.core.is_some_and(|core| !(core.local && core.remote)) =>
        {
            Action::Merge
        }
        _ => Action::Conflict,
    }
}

/// The cloud file as read.
struct Remote {
    etag: Option<String>,
    bytes: Vec<u8>,
    document: Result<ShelluneData, PortableError>,
    /// The fingerprint of the document's view for this platform.
    view_hash: Option<String>,
    /// The fingerprint of the same view without the recent connections.
    core_hash: Option<String>,
    /// What tells this version from the last synced one: the view, and — when
    /// this machine syncs passwords — the sealed block too (which the view
    /// leaves out).
    mark: String,
}

impl Remote {
    /// What the cloud holds, as the state records it, when this version stays.
    fn seen(&self) -> Seen {
        Seen {
            etag: self.etag.clone(),
            mark: self.mark.clone(),
            core_hash: self.core_hash.clone(),
        }
    }
}

/// What the cloud holds after a successful sync, as the state records it.
struct Seen {
    etag: Option<String>,
    mark: String,
    /// The fingerprint of the cloud document's view for this platform without
    /// the recent connections.
    core_hash: Option<String>,
}

enum Step {
    Done(SyncOutcome),
    Retry,
}

/// What a merge needs beyond the decision: which side changed records, and the
/// cloud's opened passwords (when passwords are synced).
struct CoreCheck {
    changes: CoreChanges,
    bundle: Option<SecretBundle>,
}

/// Syncs one data directory with the WebDAV folder its settings name.
///
/// Every call is blocking (network and disk) and meant for a background thread.
/// The settings, the state and the snapshots live in the data directory; the
/// password and passphrase in the credential store.
#[derive(Clone)]
pub struct SyncService {
    data: LocalData,
    platform: Platform,
    secrets: Arc<dyn SecretStore>,
    kdf: KdfParams,
}

impl std::fmt::Debug for SyncService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncService")
            .field("data", &self.data)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl SyncService {
    /// Sync for the data in `data`, as `platform`, with secrets in `secrets`.
    #[must_use]
    pub fn new(data: LocalData, platform: Platform, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            data,
            platform,
            secrets,
            kdf: KdfParams::default(),
        }
    }

    /// The same service sealing passwords at another key-derivation cost (tests
    /// use a cheap one).
    #[must_use]
    pub fn with_kdf(mut self, kdf: KdfParams) -> Self {
        self.kdf = kdf;
        self
    }

    /// The data directory.
    #[must_use]
    pub fn data(&self) -> &LocalData {
        &self.data
    }

    /// The platform this machine syncs as.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The saved settings; `None` when sync was never set up.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for a damaged or newer settings file.
    pub fn load_config(&self) -> Result<Option<SyncConfig>, ConfigError> {
        SyncConfigStore::at(self.data.root()).load()
    }

    /// Saves the settings. Pointing at another server or folder makes the next
    /// sync a first sync.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Write`] when they could not be written.
    pub fn save_config(&self, config: &SyncConfig) -> Result<(), ConfigError> {
        SyncConfigStore::at(self.data.root()).save(config)
    }

    /// What this machine remembers about the last sync.
    #[must_use]
    pub fn state(&self) -> SyncState {
        SyncStateStore::at(self.data.root()).load()
    }

    /// The saved WebDAV password.
    ///
    /// # Errors
    ///
    /// The credential store could not be read.
    pub fn password(&self) -> Result<Option<SecretString>, SecretError> {
        self.secrets.load_app(AppSecret::WebDavPassword)
    }

    /// Saves the WebDAV password, or forgets it with `None`.
    ///
    /// # Errors
    ///
    /// The credential store refused.
    pub fn set_password(&self, password: Option<&SecretString>) -> Result<(), SecretError> {
        set_app_secret(&*self.secrets, AppSecret::WebDavPassword, password)
    }

    /// The saved sync passphrase.
    ///
    /// # Errors
    ///
    /// The credential store could not be read.
    pub fn passphrase(&self) -> Result<Option<SecretString>, SecretError> {
        self.secrets.load_app(AppSecret::SyncPassphrase)
    }

    /// Saves the sync passphrase, or forgets it with `None`.
    ///
    /// # Errors
    ///
    /// The credential store refused.
    pub fn set_passphrase(&self, passphrase: Option<&SecretString>) -> Result<(), SecretError> {
        set_app_secret(&*self.secrets, AppSecret::SyncPassphrase, passphrase)
    }

    /// Checks settings that may not be saved yet: reaches the server, signs in,
    /// makes the folder when it is missing and writes a probe file into it.
    ///
    /// # Errors
    ///
    /// See [`WebDavClient::test`].
    pub fn test_connection(
        config: &SyncConfig,
        password: Option<&SecretString>,
    ) -> Result<(), WebDavError> {
        WebDavClient::new(&config.url, &config.remote_dir, &config.username, password)?.test()
    }

    /// Syncs in whichever direction the comparison says (automatic sync).
    ///
    /// # Errors
    ///
    /// See [`SyncError`]; the failure is also recorded in the state.
    pub fn sync(&self) -> Result<SyncOutcome, SyncError> {
        self.run(Direction::Auto)
    }

    /// "Upload now": writes this machine's data unless the cloud changed since the
    /// last sync, which is reported as a conflict instead.
    ///
    /// # Errors
    ///
    /// See [`SyncError`].
    pub fn upload(&self) -> Result<SyncOutcome, SyncError> {
        self.run(Direction::Upload)
    }

    /// "Pull from the cloud": applies the cloud's data unless this machine has
    /// changes not yet synced, which is reported as a conflict instead.
    ///
    /// # Errors
    ///
    /// See [`SyncError`]; [`SyncError::NothingInCloud`] when there is no file.
    pub fn pull(&self) -> Result<SyncOutcome, SyncError> {
        self.run(Direction::Pull)
    }

    /// Settles a conflict.
    ///
    /// # Errors
    ///
    /// See [`SyncError`].
    pub fn resolve(&self, resolution: Resolution) -> Result<SyncOutcome, SyncError> {
        if resolution == Resolution::Cancel {
            return self.record(Ok(SyncOutcome::ConflictKept));
        }
        let outcome = self.connect().and_then(|(config, client)| {
            self.repeat(|service| match resolution {
                Resolution::UseRemote => service.use_remote(&config, &client),
                Resolution::UseLocal => service.use_local(&config, &client),
                Resolution::Cancel => Ok(Step::Done(SyncOutcome::ConflictKept)),
            })
        });
        self.record(outcome)
    }

    /// Settles the vault conflicts of a [`SyncOutcome::Conflict`] one record at
    /// a time (ADR-5, ADR-8): every conflict of the report carries a choice, or
    /// the conflict stands and nothing is applied. With every choice given, the
    /// merged vault is sealed once, uploaded and applied as one — a failure
    /// along the way keeps the last usable state on both sides, and no partial
    /// vault is ever written.
    ///
    /// # Errors
    ///
    /// See [`SyncError`].
    pub fn resolve_vault(&self, choices: &[VaultChoice]) -> Result<SyncOutcome, SyncError> {
        let outcome = self.connect().and_then(|(config, client)| {
            self.repeat(|service| service.attempt_with(Direction::Auto, &config, &client, choices))
        });
        self.record(outcome)
    }

    fn run(&self, direction: Direction) -> Result<SyncOutcome, SyncError> {
        let outcome = self.connect().and_then(|(config, client)| {
            self.repeat(|service| service.attempt(direction, &config, &client))
        });
        self.record(outcome)
    }

    fn repeat(
        &self,
        mut step: impl FnMut(&Self) -> Result<Step, SyncError>,
    ) -> Result<SyncOutcome, SyncError> {
        for _ in 0..ATTEMPTS {
            if let Step::Done(outcome) = step(self)? {
                return Ok(outcome);
            }
        }
        Err(SyncError::Busy)
    }

    /// Writes how the attempt ended into the state.
    fn record(&self, outcome: Result<SyncOutcome, SyncError>) -> Result<SyncOutcome, SyncError> {
        if matches!(outcome, Err(SyncError::NotConfigured)) {
            return outcome;
        }
        let store = SyncStateStore::at(self.data.root());
        let mut state = store.load();
        let now = now_unix();
        state.last_time_unix = Some(now);
        match &outcome {
            Ok(done) => {
                state.last_result = Some(done.last_result());
                if !matches!(done, SyncOutcome::Conflict(_) | SyncOutcome::ConflictKept) {
                    state.last_success_unix = Some(now);
                }
            }
            Err(error) => {
                state.last_result = Some(LastResult::Failed {
                    reason: error.to_string(),
                });
            }
        }
        match store.save(&state) {
            Err(error) if outcome.is_ok() => Err(SyncError::Config(error)),
            _ => outcome,
        }
    }

    fn connect(&self) -> Result<(SyncConfig, WebDavClient), SyncError> {
        let config = self
            .load_config()?
            .filter(SyncConfig::is_configured)
            .ok_or(SyncError::NotConfigured)?;
        let password = self.password().map_err(SyncError::Credentials)?;
        let client = WebDavClient::new(
            &config.url,
            &config.remote_dir,
            &config.username,
            password.as_ref(),
        )?;
        Ok((config, client))
    }

    fn attempt(
        &self,
        direction: Direction,
        config: &SyncConfig,
        client: &WebDavClient,
    ) -> Result<Step, SyncError> {
        self.attempt_with(direction, config, client, &[])
    }

    /// One decision over the data both sides hold. `choices` are the user's
    /// answers to the vault conflicts of the last attempt; without them a
    /// vault conflict stands and nothing of the vault is applied.
    fn attempt_with(
        &self,
        direction: Direction,
        config: &SyncConfig,
        client: &WebDavClient,
        choices: &[VaultChoice],
    ) -> Result<Step, SyncError> {
        let remote_id = client.file_url();
        let state = self.state();
        let known = state.knows(&remote_id);
        let local = self.data.load().map_err(SyncError::Local)?;
        // Opening the vault takes no network, and a vault no secret of this
        // machine can open is an error rather than an absence: an unusable
        // vault never counts as an empty one (R5).
        let local_vault = if config.sync_secrets {
            self.open_local_vault()?
        } else {
            None
        };
        let mine = self.fingerprint(config, &local, local_vault.as_ref())?;
        // Passwords were just switched on here: the cloud may already carry them
        // (from another machine), and this machine's own set is not newer.
        let adopting_secrets = known
            && config.sync_secrets
            && local_vault.is_none()
            && state.last_secrets_hash.is_none();
        let secrets_changed = known && !adopting_secrets && state.last_secrets_hash != mine.secrets;
        // The vault has its own half of "this machine changed": a record, a
        // tombstone or a trust entry is a change even when no host record moved.
        // A machine with no vault file is not "changed" for lacking one — a
        // vault that appears is a change, a vault that never existed is not.
        let vault_changed_here = known
            && config.sync_secrets
            && local_vault.as_ref().is_some_and(|vault| {
                state.last_vault_hash.as_deref() != Some(vault.fingerprint.as_str())
            });
        let local_changed = !known
            || state.last_synced_hash.as_deref() != Some(mine.view.as_str())
            || secrets_changed
            || vault_changed_here;

        let Some(file) = client.stat()? else {
            return match direction {
                Direction::Pull => Err(SyncError::NothingInCloud),
                // Nothing on either side: nothing to create yet.
                Direction::Auto if local.is_empty() && local_vault.is_none() => {
                    Ok(Step::Done(SyncOutcome::UpToDate))
                }
                Direction::Auto | Direction::Upload => {
                    if let Some(local_vault) = local_vault {
                        // The first upload with a vault: the document becomes a
                        // version 2 one that carries the ciphertext.
                        let source = self.vault_source()?;
                        let state = self.state();
                        let device_id = state.device_id.clone().unwrap_or_else(new_device_id);
                        let merged = merge_vault(
                            Some(&local_vault),
                            None,
                            None,
                            choices,
                            &device_id,
                            &self.empty_document(),
                        )?;
                        let VaultPlan::Write(write) = merged.plan else {
                            return Ok(Step::Done(SyncOutcome::UpToDate));
                        };
                        self.upload_document(
                            config,
                            client,
                            &remote_id,
                            &local,
                            None,
                            None,
                            DocumentVault::Write {
                                write: &write,
                                source: &source,
                                device_id: &device_id,
                            },
                            "sync-upload",
                        )
                    } else {
                        self.upload_to(config, client, &remote_id, &local, &mine, None)
                    }
                }
            };
        };
        let etag_unchanged = known && file.etag.is_some() && file.etag == state.last_remote_etag;
        if etag_unchanged && !local_changed && !adopting_secrets {
            return Ok(Step::Done(SyncOutcome::UpToDate));
        }
        let Some(remote) = self.fetch(config, client)? else {
            // Removed between the two requests.
            return Ok(Step::Retry);
        };
        let document = remote
            .document
            .as_ref()
            .map_err(|error| SyncError::RemoteDocument(error.clone()))?;

        // The vault travels whenever the user asked for it and one exists on
        // either side; everything below is the arrangement without one.
        if config.sync_secrets && (local_vault.is_some() || document.has_vault()) {
            return self.attempt_vault(
                direction,
                config,
                client,
                &remote_id,
                &state,
                known,
                etag_unchanged,
                &local,
                local_vault,
                &remote,
                document,
                &mine,
                choices,
            );
        }
        // A cloud that carried a vault this machine has a baseline for, and no
        // longer does, is either an abnormal rollback or an old client that
        // overwrote it: neither is applied, and neither is overwritten back.
        if config.sync_secrets && known && state.vault.is_some() && !document.has_vault() {
            return Err(vault_gone());
        }
        if config.sync_secrets {
            // The sealed-password arrangement needs the passphrase; a machine
            // that has none is told so here, before anything is written.
            self.sync_passphrase()?;
        }
        let remote_hash = remote.view_hash.clone().unwrap_or_default();
        // Switching passwords on where the cloud has none is this machine's change.
        let local_changed = local_changed || (adopting_secrets && !document.has_secrets());
        let remote_changed = !known
            || (adopting_secrets && document.has_secrets())
            || (!etag_unchanged && state.last_remote_hash.as_deref() != Some(remote.mark.as_str()));
        let check = if direction == Direction::Auto
            && known
            && local_changed
            && remote_changed
            && !adopting_secrets
        {
            self.check_core(config, &state, &mine, secrets_changed, document, &remote)?
        } else {
            None
        };
        let facts = Facts {
            first_sync: !known,
            local_changed,
            remote_changed,
            local_empty: local.is_empty(),
            remote_empty: document_is_empty(document),
            same_view: remote_hash == mine.view && !secrets_changed && !adopting_secrets,
            core: check.as_ref().map(|check| check.changes),
        };
        let conflict = || -> Result<Step, SyncError> {
            Ok(Step::Done(SyncOutcome::Conflict(ConflictReport {
                first_sync: !known,
                remote_platform: document.source_platform().to_owned(),
                remote_app_version: document.source_app_version().to_owned(),
                remote_created_unix: document.created_unix(),
                vault: Vec::new(),
            })))
        };
        match decide(direction, facts) {
            Action::Nothing => {
                self.remember_vault(&remote_id, &remote.seen(), &mine, None)?;
                Ok(Step::Done(SyncOutcome::UpToDate))
            }
            Action::Upload => {
                self.upload_to(config, client, &remote_id, &local, &mine, Some(&remote))
            }
            Action::Pull => self.apply_remote(config, &remote_id, &local, &remote, "sync-pull"),
            Action::Merge => match check {
                Some(check) => self.merge(
                    config,
                    client,
                    &remote_id,
                    &local,
                    &remote,
                    check,
                    DocumentVault::None,
                ),
                None => conflict(),
            },
            Action::Conflict => conflict(),
        }
    }

    /// A placeholder document for a merge with no cloud side; only its source
    /// facts are read.
    fn empty_document(&self) -> ShelluneData {
        ShelluneData::export(
            &LocalState::default(),
            &ExportOptions::new(self.platform),
            &*self.secrets,
            None,
        )
        .expect("an empty state exports")
        .document
    }

    /// The vault half of one sync (R5, R6, R8): both sides opened with the
    /// selected secret, checked against the baseline, merged record by record,
    /// and written as one or stopped at a conflict.
    #[allow(clippy::too_many_arguments)]
    fn attempt_vault(
        &self,
        direction: Direction,
        config: &SyncConfig,
        client: &WebDavClient,
        remote_id: &str,
        state: &SyncState,
        known: bool,
        etag_unchanged: bool,
        local: &LocalState,
        local_vault: Option<LocalVault>,
        remote: &Remote,
        document: &ShelluneData,
        mine: &Fingerprint,
        choices: &[VaultChoice],
    ) -> Result<Step, SyncError> {
        let source = self.vault_source()?;
        let remote_vault = match document.vault() {
            Some(cipher) => {
                let content = cipher
                    .open(&source.key_source())
                    .map_err(SyncError::Vault)?;
                let fingerprint = vault_fingerprint(&content);
                Some(RemoteVault {
                    content,
                    cipher: cipher.clone(),
                    fingerprint,
                })
            }
            None => None,
        };
        let baseline = known.then_some(state.vault.as_ref()).flatten();

        // A version 1 document still carrying sealed passwords has not been
        // migrated into a vault (R9): replacing it would drop those passwords
        // from the cloud without anyone deciding to, so the user settles it.
        if document.has_secrets() {
            return Ok(Step::Done(SyncOutcome::Conflict(ConflictReport {
                first_sync: !known,
                remote_platform: document.source_platform().to_owned(),
                remote_app_version: document.source_app_version().to_owned(),
                remote_created_unix: document.created_unix(),
                vault: Vec::new(),
            })));
        }
        self.check_vault_baseline(baseline, local_vault.as_ref(), remote_vault.as_ref())?;

        let device_id = state.device_id.clone().unwrap_or_else(new_device_id);
        let merged = merge_vault(
            local_vault.as_ref(),
            remote_vault.as_ref(),
            baseline,
            choices,
            &device_id,
            document,
        )?;

        // The document facts, without the vault: the vault has its own plan,
        // and records only the vault changed never become a document conflict
        // (different records merge, ADR-5).
        let doc_local_changed =
            !known || state.last_synced_hash.as_deref() != Some(mine.view.as_str());
        let doc_remote_changed = !known
            || (!etag_unchanged && state.last_remote_hash.as_deref() != Some(remote.mark.as_str()));
        let vault_unchanged = !matches!(merged.plan, VaultPlan::Write(_));
        let check =
            if direction == Direction::Auto && known && doc_local_changed && doc_remote_changed {
                check_core_docs(state, mine, remote)
            } else {
                None
            };
        let vault_holds_records =
            |content: &VaultContent| content.keys().count() + content.credentials().count() > 0;
        let facts = Facts {
            first_sync: !known,
            local_changed: doc_local_changed,
            remote_changed: doc_remote_changed,
            local_empty: local.is_empty()
                && !local_vault
                    .as_ref()
                    .is_some_and(|vault| vault_holds_records(&vault.content)),
            remote_empty: document_is_empty(document) && remote_vault.is_none(),
            same_view: remote.view_hash.as_deref() == Some(mine.view.as_str()) && vault_unchanged,
            core: check,
        };
        let conflict = |vault: Vec<VaultConflict>| -> Result<Step, SyncError> {
            Ok(Step::Done(SyncOutcome::Conflict(ConflictReport {
                first_sync: !known,
                remote_platform: document.source_platform().to_owned(),
                remote_app_version: document.source_app_version().to_owned(),
                remote_created_unix: document.created_unix(),
                vault,
            })))
        };
        // A conflict left unsettled stops the whole vault: nothing is applied,
        // here or in the cloud (ADR-5).
        if !merged.conflicts.is_empty() {
            return conflict(merged.conflicts);
        }
        let record_of = |plan: &VaultPlan| -> Option<VaultRemember<'_>> {
            match plan {
                VaultPlan::Absent => None,
                VaultPlan::Unchanged(settled) => Some(VaultRemember {
                    device_id: &device_id,
                    vault_id: settled.vault_id.clone(),
                    content: settled.content.clone(),
                    remote_fingerprint: None,
                }),
                // A write records itself once it has been written.
                VaultPlan::Write(_) => None,
            }
        };
        match (decide(direction, facts), &merged.plan) {
            (Action::Nothing, VaultPlan::Absent | VaultPlan::Unchanged(_)) => {
                self.remember_vault(remote_id, &remote.seen(), mine, record_of(&merged.plan))?;
                Ok(Step::Done(SyncOutcome::UpToDate))
            }
            (Action::Conflict, _) | (Action::Upload, VaultPlan::Absent) => conflict(Vec::new()),
            (Action::Upload, VaultPlan::Unchanged(settled)) => {
                // This machine's document changed; the vault travels as it
                // stands, in the block the cloud already holds.
                let remote_vault = remote_vault.expect("an unchanged vault has a cloud side");
                self.upload_document(
                    config,
                    client,
                    remote_id,
                    local,
                    None,
                    Some(remote),
                    DocumentVault::Carry {
                        cipher: &remote_vault.cipher,
                        settled,
                        device_id: &device_id,
                    },
                    "sync-upload",
                )
            }
            (Action::Upload | Action::Nothing, VaultPlan::Write(write)) => {
                // This machine's records changed, or only its vault did: the
                // document is written around the new ciphertext.
                self.upload_document(
                    config,
                    client,
                    remote_id,
                    local,
                    None,
                    Some(remote),
                    DocumentVault::Write {
                        write,
                        source: &source,
                        device_id: &device_id,
                    },
                    "sync-upload",
                )
            }
            (Action::Pull, plan) => {
                match plan {
                    VaultPlan::Absent => conflict(Vec::new()),
                    VaultPlan::Unchanged(settled) => self.apply_remote_vault(
                        remote_id,
                        local,
                        remote,
                        document,
                        &settled.content,
                        &settled.vault_id,
                        false,
                        &source,
                        &device_id,
                        baseline.is_none(),
                        "sync-pull",
                    ),
                    VaultPlan::Write(write) if write.origin == VaultOrigin::RemoteOnly => {
                        // The cloud's vault changed and this machine's did not:
                        // the cloud's content applies, with this machine's own
                        // wrap added for local unsealing.
                        self.apply_remote_vault(
                            remote_id,
                            local,
                            remote,
                            document,
                            &write.content,
                            &write.vault_id,
                            true,
                            &source,
                            &device_id,
                            baseline.is_none(),
                            "sync-pull",
                        )
                    }
                    VaultPlan::Write(write) => {
                        // The cloud's document changed and so did this vault:
                        // the document's plan applies and the merged vault
                        // travels back with it.
                        let plan = document
                            .plan(local, self.platform, ApplyMode::Replace, None)
                            .map_err(SyncError::RemoteDocument)?;
                        self.upload_document(
                            config,
                            client,
                            remote_id,
                            local,
                            Some(&plan),
                            Some(remote),
                            DocumentVault::Write {
                                write,
                                source: &source,
                                device_id: &device_id,
                            },
                            "sync-pull",
                        )
                    }
                }
            }
            (Action::Merge, plan) => {
                let Some(changes) = check else {
                    return conflict(Vec::new());
                };
                let travel = match plan {
                    VaultPlan::Absent | VaultPlan::Unchanged(_) if remote_vault.is_none() => {
                        DocumentVault::None
                    }
                    VaultPlan::Unchanged(settled) => DocumentVault::Carry {
                        cipher: &remote_vault
                            .expect("an unchanged vault has a cloud side")
                            .cipher,
                        settled,
                        device_id: &device_id,
                    },
                    VaultPlan::Write(write) => DocumentVault::Write {
                        write,
                        source: &source,
                        device_id: &device_id,
                    },
                    VaultPlan::Absent => DocumentVault::None,
                };
                self.merge(
                    config,
                    client,
                    remote_id,
                    local,
                    remote,
                    CoreCheck {
                        changes,
                        bundle: None,
                    },
                    travel,
                )
            }
        }
    }

    /// The secret that protects the travelling vault (R5, ADR-2): the non-empty
    /// WebDAV password when there is one, otherwise the sync passphrase.
    ///
    /// A password that cannot be read is not "no password": refusing beats
    /// quietly falling back to the weaker arrangement. Reusing the WebDAV
    /// password is the user's confirmed choice — the server sees it in every
    /// Basic authorization header, so it can decrypt and forge valid vault
    /// ciphertext; this mode is not zero-knowledge towards the WebDAV server,
    /// and neither this code nor any wording around it may claim it is. A
    /// passwordless server never receives the sync passphrase.
    fn vault_source(&self) -> Result<VaultSource, SyncError> {
        let password = self.password().map_err(SyncError::Credentials)?;
        if let Some(password) = password.filter(|password| !password.expose().trim().is_empty()) {
            return Ok(VaultSource::WebDavPassword(password));
        }
        let passphrase = self.passphrase().map_err(SyncError::Credentials)?;
        passphrase
            .filter(|passphrase| !passphrase.expose().trim().is_empty())
            .map(VaultSource::SyncPassphrase)
            .ok_or(SyncError::PassphraseMissing)
    }

    /// This machine's vault, opened with whichever of its secrets works: the
    /// local one first, then the travelling ones. `Ok(None)` when there is no
    /// vault file; a file no secret opens is an error, never an absence.
    fn open_local_vault(&self) -> Result<Option<LocalVault>, SyncError> {
        let store = VaultStore::at(self.data.root());
        let Some(cipher) = store.read_cipher().map_err(SyncError::Vault)? else {
            return Ok(None);
        };
        let vault_id = cipher.vault_id().to_owned();
        let mut sources = vec![UnsealSource::LocalMachine];
        if let Ok(Some(password)) = self.secrets.load_app(AppSecret::WebDavPassword)
            && !password.expose().trim().is_empty()
        {
            sources.push(UnsealSource::WebDavPassword(password));
        }
        if let Ok(Some(passphrase)) = self.secrets.load_app(AppSecret::SyncPassphrase)
            && !passphrase.expose().trim().is_empty()
        {
            sources.push(UnsealSource::SyncPassphrase(passphrase));
        }
        let mut opened = None;
        for source in &sources {
            match store.open(source, &*self.secrets) {
                Ok(content) => {
                    opened = Some(content);
                    break;
                }
                Err(VaultStoreError::Sealed | VaultStoreError::NoLocalSecret) => {}
                // A failing system store is not a wrong secret; it is named.
                Err(error) => return Err(SyncError::Vault(error)),
            }
        }
        let content = opened.ok_or(SyncError::Vault(VaultStoreError::Sealed))?;
        Ok(Some(LocalVault {
            fingerprint: vault_fingerprint(&content),
            vault_id,
            content,
        }))
    }

    /// Seals `content` for the travel: under the selected secret, and under
    /// this machine's own local secret when it has one (a wrap that opens
    /// nothing for anyone without this machine's system credential store).
    fn seal_vault(
        &self,
        content: &VaultContent,
        vault_id: Option<&str>,
        source: &VaultSource,
    ) -> Result<VaultCipher, SyncError> {
        let mut sources = Vec::new();
        if let Ok(Some(secret)) = self.secrets.load_app(AppSecret::VaultLocalUnseal) {
            sources.push(shellune_storage::KeySource::LocalMachine(secret));
        }
        sources.push(source.key_source());
        VaultCipher::seal(content, vault_id, &sources, self.kdf).map_err(SyncError::Vault)
    }

    /// The baseline checks (R6, R8): the vault both sides synced must still
    /// exist on both sides — a deletion is a tombstone, never a disappearance —
    /// and a cloud older than what this machine applied is an abnormal
    /// rollback that stops automatic sync entirely.
    fn check_vault_baseline(
        &self,
        baseline: Option<&VaultBaseline>,
        local: Option<&LocalVault>,
        remote: Option<&RemoteVault>,
    ) -> Result<(), SyncError> {
        let Some(base) = baseline else {
            return Ok(());
        };
        let lost = |side: &str| {
            SyncError::Vault(VaultStoreError::Invalid {
                reason: format!(
                    "the encrypted vault of {side} no longer holds every record it last synced; \
                     refusing to apply or overwrite anything"
                ),
            })
        };
        match remote {
            None => return Err(vault_gone()),
            Some(remote) => {
                let found = vault_global(&remote.content);
                if found < base.global_counter {
                    return Err(SyncError::Rollback {
                        found,
                        applied: base.global_counter,
                    });
                }
                if base.keys.keys().any(|id| remote.content.key(*id).is_none())
                    || base
                        .credentials
                        .keys()
                        .any(|id| remote.content.credential(*id).is_none())
                {
                    return Err(lost("the cloud document"));
                }
            }
        }
        match local {
            None => Err(SyncError::Vault(VaultStoreError::Invalid {
                reason: "this machine no longer holds the encrypted vault it last synced; \
                         refusing to apply or overwrite anything"
                    .to_owned(),
            })),
            Some(local) => {
                if base.keys.keys().any(|id| local.content.key(*id).is_none())
                    || base
                        .credentials
                        .keys()
                        .any(|id| local.content.credential(*id).is_none())
                {
                    return Err(lost("this machine"));
                }
                Ok(())
            }
        }
    }

    /// Writes the document of `final_state` (or of `plan`, when the local data
    /// is applied as part of the same step) with the vault block `vault` names,
    /// then applies and saves, in the order that keeps a failure harmless: the
    /// upload first (a refused write decides again), the document apply second
    /// (snapshotted, refused when the data changed), the vault last — validated
    /// against the host book before anything was written anywhere.
    #[allow(clippy::too_many_arguments)]
    fn upload_document(
        &self,
        config: &SyncConfig,
        client: &WebDavClient,
        remote_id: &str,
        local: &LocalState,
        plan: Option<&ApplyPlan>,
        base: Option<&Remote>,
        vault: DocumentVault<'_>,
        reason: &str,
    ) -> Result<Step, SyncError> {
        let final_state = plan.map(|plan| plan.result()).unwrap_or(local);
        let mut options = ExportOptions::new(self.platform);
        options.kdf = self.kdf;
        let mut record: Option<VaultRemember<'_>> = None;
        let mut save: Option<(VaultContent, UnsealSource)> = None;
        match &vault {
            DocumentVault::None => {
                if config.sync_secrets {
                    options = options.with_passphrase(self.sync_passphrase()?);
                }
                // A cloud document that carries a vault keeps it when this
                // machine does not sync the vault: dropping the block would
                // delete the only ciphertext without the user's explicit
                // choice (R5).
                let carry = base
                    .and_then(|remote| remote.document.as_ref().ok())
                    .and_then(|document| document.vault().cloned());
                if let Some(cipher) = carry {
                    options = options.with_vault(cipher);
                }
            }
            DocumentVault::Carry {
                cipher,
                settled,
                device_id,
            } => {
                options = options.with_vault((*cipher).clone());
                record = Some(VaultRemember {
                    device_id,
                    vault_id: settled.vault_id.clone(),
                    content: settled.content.clone(),
                    remote_fingerprint: None,
                });
            }
            DocumentVault::Write {
                write,
                source,
                device_id,
            } => {
                let mut content = write.content.clone();
                let hosts = ssh_hosts(final_state);
                content.set_references(&HostReference::snapshot(&hosts));
                // The whole reference set is validated against the host book
                // the apply ends with, as one set, before anything is written
                // anywhere: a binding that cannot hold fails the whole vault
                // (R8), and a half vault is never applied.
                content.validate_for(&hosts).map_err(SyncError::Vault)?;
                let cipher = self.seal_vault(&content, Some(&write.vault_id), source)?;
                options = options.with_vault(cipher);
                save = Some((content.clone(), source.unseal_source()));
                record = Some(VaultRemember {
                    device_id,
                    vault_id: write.vault_id.clone(),
                    content,
                    remote_fingerprint: None,
                });
            }
        }
        let base_document = base.and_then(|remote| remote.document.as_ref().ok());
        let export = ShelluneData::export(final_state, &options, &*self.secrets, base_document)
            .map_err(SyncError::Local)?;
        let bytes = export.document.to_json().map_err(SyncError::Local)?;
        let Some(etag) = put(client, base, &bytes)? else {
            return Ok(Step::Retry);
        };
        let applied = match plan {
            Some(plan) => Some(match plan.apply(&self.data, &*self.secrets, reason) {
                Err(PortableError::Changed) => return Ok(Step::Retry),
                other => other.map_err(SyncError::Local)?,
            }),
            None => None,
        };
        if let Some((content, unseal)) = save {
            let end = applied.as_ref().map_or(local, |applied| &applied.state);
            VaultStore::at(self.data.root())
                .save(
                    &content,
                    &ssh_hosts(end),
                    &[UnsealSource::LocalMachine, unseal],
                    self.kdf,
                    &*self.secrets,
                )
                .map_err(SyncError::Vault)?;
        }
        let seen = self
            .seen_upload(config, &export.document, etag, &bytes)
            .map_err(SyncError::Local)?;
        let end = applied.as_ref().map_or(local, |applied| &applied.state);
        let mut mine = self.document_fingerprint(end)?;
        let restored = record.as_ref().map(|record| VaultRestored {
            global_counter: vault_global(&record.content),
            live_records: vault_live_records(&record.content),
            first_sync: false,
        });
        if let Some(record) = &record {
            mine.vault = Some(vault_fingerprint(&record.content));
        }
        self.remember_vault(remote_id, &seen, &mine, record)?;
        Ok(Step::Done(match applied {
            Some(applied) => SyncOutcome::Merged(Box::new(pull_report(
                plan.expect("an applied document implies its plan"),
                applied,
                false,
                restored,
            ))),
            None => SyncOutcome::Uploaded(UploadReport {
                created: base.is_none(),
                secrets_included: export.secrets_included,
                secrets_unreadable: export.secrets_unreadable,
                secrets_missing: export.secrets_missing,
                remote_snapshot: None,
            }),
        }))
    }

    /// Applies the cloud document and its vault here, as one replacement: the
    /// vault's reference set is validated against the host book the apply ends
    /// with before anything is written, so a vault whose bindings cannot hold
    /// fails whole (R8) and this machine keeps what it had.
    #[allow(clippy::too_many_arguments)]
    fn apply_remote_vault(
        &self,
        remote_id: &str,
        local: &LocalState,
        remote: &Remote,
        document: &ShelluneData,
        content: &VaultContent,
        vault_id: &str,
        save: bool,
        source: &VaultSource,
        device_id: &str,
        first_vault_sync: bool,
        reason: &str,
    ) -> Result<Step, SyncError> {
        let plan = document
            .plan(local, self.platform, ApplyMode::Replace, None)
            .map_err(SyncError::RemoteDocument)?;
        let mut content = content.clone();
        let hosts = ssh_hosts(plan.result());
        content.set_references(&HostReference::snapshot(&hosts));
        content.validate_for(&hosts).map_err(SyncError::Vault)?;
        let applied = match plan.apply(&self.data, &*self.secrets, reason) {
            Err(PortableError::Changed) => return Ok(Step::Retry),
            other => other.map_err(SyncError::Local)?,
        };
        if save {
            // The local file gets this machine's own wrap beside the travelling
            // one; a first adoption creates the local unseal entry on the way.
            VaultStore::at(self.data.root())
                .save(
                    &content,
                    &ssh_hosts(&applied.state),
                    &[UnsealSource::LocalMachine, source.unseal_source()],
                    self.kdf,
                    &*self.secrets,
                )
                .map_err(SyncError::Vault)?;
        }
        let mut mine = self.document_fingerprint(&applied.state)?;
        mine.vault = Some(vault_fingerprint(&content));
        self.remember_vault(
            remote_id,
            &remote.seen(),
            &mine,
            Some(VaultRemember {
                device_id,
                vault_id: vault_id.to_owned(),
                content: content.clone(),
                remote_fingerprint: Some(vault_fingerprint(&content)),
            }),
        )?;
        let restored = VaultRestored {
            global_counter: vault_global(&content),
            live_records: vault_live_records(&content),
            // A first adoption has no baseline of its own: the version it
            // landed on is shown, and nothing is claimed about recognising
            // rollbacks of the past (R6).
            first_sync: first_vault_sync,
        };
        Ok(Step::Done(SyncOutcome::Pulled(Box::new(pull_report(
            &plan,
            applied,
            false,
            Some(restored),
        )))))
    }

    /// Tells whether each side changed more than its recent connections since the
    /// last sync: this machine's data against what it held then, the cloud's
    /// document against what the cloud held then (the two views differ when
    /// another platform wrote it). `None` when that cannot be told: the last sync
    /// was recorded by a build that did not keep the data without recents, or the
    /// cloud's passwords do not open with this machine's passphrase.
    ///
    /// Both sides holding the same records and passwords (only the recents
    /// differ) counts as neither having changed them, whatever the baseline says:
    /// a merge then loses nothing.
    fn check_core(
        &self,
        config: &SyncConfig,
        state: &SyncState,
        mine: &Fingerprint,
        secrets_changed: bool,
        document: &ShelluneData,
        remote: &Remote,
    ) -> Result<Option<CoreCheck>, SyncError> {
        let (Some(baseline), Some(remote_baseline)) = (
            state.last_synced_core_hash.as_deref(),
            state.last_remote_core_hash.as_deref(),
        ) else {
            return Ok(None);
        };
        let mut remote_core = remote.core_hash.as_deref() != Some(remote_baseline);
        let mut same_core = remote.core_hash.as_deref() == Some(mine.core.as_str());
        let mut bundle = None;
        if config.sync_secrets {
            // The sealed block is sealed afresh on every upload, so whether the
            // passwords in it changed is only known once it is opened.
            let passphrase = self.sync_passphrase()?;
            let Ok(opened) = document.open_secrets(&passphrase) else {
                return Ok(None);
            };
            let records = document.shared().hosts().records();
            let theirs = secrets_digest(
                &passphrase,
                records.iter().filter_map(|record| {
                    opened.get(record.id()).map(|secret| (record.id(), secret))
                }),
            );
            remote_core |= state.last_secrets_hash.as_deref() != Some(theirs.as_str());
            same_core &= mine.secrets.as_deref() == Some(theirs.as_str());
            bundle = Some(opened);
        } else {
            // Turning password sync off is a change here (the cloud must drop
            // the sealed block), even when the records match the cloud's.
            same_core &= !secrets_changed;
        }
        let changes = if same_core {
            CoreChanges {
                local: false,
                remote: false,
            }
        } else {
            CoreChanges {
                local: mine.core != baseline || secrets_changed,
                remote: remote_core,
            }
        };
        Ok(Some(CoreCheck { changes, bundle }))
    }

    /// Merges both sides when at least one only connected to hosts: the records
    /// of the side that changed them (the cloud's when neither did), the later
    /// recent connection per host.
    ///
    /// The result goes to the cloud first (If-Match), then is written here (after
    /// a snapshot), so a failed or refused upload leaves this machine untouched
    /// and decides again. A save made here meanwhile refuses the write and
    /// decides again too. A vault the merge changes rules out carrying the
    /// cloud's document: the result is exported with the new ciphertext.
    #[allow(clippy::too_many_arguments)]
    fn merge(
        &self,
        config: &SyncConfig,
        client: &WebDavClient,
        remote_id: &str,
        local: &LocalState,
        remote: &Remote,
        check: CoreCheck,
        vault: DocumentVault<'_>,
    ) -> Result<Step, SyncError> {
        let document = remote
            .document
            .as_ref()
            .map_err(|error| SyncError::RemoteDocument(error.clone()))?;
        let recents =
            merge_recent_connections(local.hosts.recents(), document.shared().hosts().recents());
        let from_remote = check.changes.remote || !check.changes.local;
        if matches!(vault, DocumentVault::Write { .. }) {
            // The vault changed, so the cloud never already holds the result.
            let plan = if from_remote {
                document
                    .plan(local, self.platform, ApplyMode::Replace, None)
                    .map_err(SyncError::RemoteDocument)?
            } else {
                ApplyPlan::with_recents(local, self.platform, &recents).map_err(SyncError::Local)?
            };
            return self.upload_document(
                config,
                client,
                remote_id,
                local,
                Some(&plan),
                Some(remote),
                vault,
                "sync-merge",
            );
        }
        let mut options = ExportOptions::new(self.platform);
        options.kdf = self.kdf;
        match &vault {
            DocumentVault::Carry { cipher, .. } => {
                options = options.with_vault((*cipher).clone());
            }
            DocumentVault::Write { .. } => unreachable!("handled above"),
            DocumentVault::None => {
                if config.sync_secrets {
                    options = options.with_passphrase(self.sync_passphrase()?);
                }
            }
        }
        let (plan, upload) = if from_remote {
            // When the cloud's recents are the later ones it already holds the
            // result, in its own order.
            let theirs = merge_recent_connections(document.shared().hosts().recents(), &[]);
            let mut merged = if theirs == recents {
                None
            } else {
                // The cloud's document with the merged recents; its sealed
                // passwords, its vault block and the other platforms' sections
                // travel unchanged.
                Some(
                    document
                        .with_recents(&recents)
                        .map_err(SyncError::RemoteDocument)?,
                )
            };
            let plan = merged
                .as_ref()
                .unwrap_or(document)
                .plan(
                    local,
                    self.platform,
                    ApplyMode::Replace,
                    check.bundle.as_ref(),
                )
                .map_err(SyncError::RemoteDocument)?;
            if let Some(merged) = &mut merged {
                merged.restamp(&options);
            }
            (plan, merged)
        } else {
            let plan = ApplyPlan::with_recents(local, self.platform, &recents)
                .map_err(SyncError::Local)?;
            let export =
                ShelluneData::export(plan.result(), &options, &*self.secrets, Some(document))
                    .map_err(SyncError::Local)?;
            (plan, Some(export.document))
        };
        let seen = match upload {
            Some(upload) => {
                let bytes = upload.to_json().map_err(SyncError::Local)?;
                let Some(etag) = put(client, Some(remote), &bytes)? else {
                    return Ok(Step::Retry);
                };
                self.seen_upload(config, &upload, etag, &bytes)
                    .map_err(SyncError::Local)?
            }
            None => remote.seen(),
        };
        let applied = match plan.apply(&self.data, &*self.secrets, "sync-merge") {
            Err(PortableError::Changed) => return Ok(Step::Retry),
            other => other.map_err(SyncError::Local)?,
        };
        let mut mine = self.document_fingerprint(&applied.state)?;
        let record = match &vault {
            DocumentVault::Carry {
                settled, device_id, ..
            } => {
                mine.vault = Some(vault_fingerprint(&settled.content));
                Some(VaultRemember {
                    device_id,
                    vault_id: settled.vault_id.clone(),
                    content: settled.content.clone(),
                    remote_fingerprint: None,
                })
            }
            DocumentVault::None => {
                // The legacy arrangement: the sealed passwords are part of the
                // fingerprint, with the passphrase that sealed them.
                if config.sync_secrets {
                    mine.secrets =
                        Some(self.legacy_secrets_digest(&self.sync_passphrase()?, &applied.state));
                }
                None
            }
            DocumentVault::Write { .. } => unreachable!("handled above"),
        };
        self.remember_vault(remote_id, &seen, &mine, record)?;
        Ok(Step::Done(SyncOutcome::Merged(Box::new(pull_report(
            &plan,
            applied,
            from_remote && document.has_secrets() && !config.sync_secrets,
            None,
        )))))
    }

    fn use_remote(&self, config: &SyncConfig, client: &WebDavClient) -> Result<Step, SyncError> {
        let local = self.data.load().map_err(SyncError::Local)?;
        let remote = self
            .fetch(config, client)?
            .ok_or(SyncError::NothingInCloud)?;
        let document = remote
            .document
            .as_ref()
            .map_err(|error| SyncError::RemoteDocument(error.clone()))?;
        let local_vault = if config.sync_secrets {
            self.open_local_vault()?
        } else {
            None
        };
        // The user chose the cloud as a whole; the vault follows that choice.
        // A wrong secret, a vanished vault and an abnormal rollback still
        // refuse — an explicit "use the cloud" is not a choice to roll back to
        // an older one (R6).
        if config.sync_secrets && (local_vault.is_some() || document.has_vault()) {
            let source = self.vault_source()?;
            let remote_vault = match document.vault() {
                Some(cipher) => {
                    let content = cipher
                        .open(&source.key_source())
                        .map_err(SyncError::Vault)?;
                    let fingerprint = vault_fingerprint(&content);
                    Some(RemoteVault {
                        content,
                        cipher: cipher.clone(),
                        fingerprint,
                    })
                }
                None => None,
            };
            let state = self.state();
            let known = state.knows(&client.file_url());
            let baseline = known.then_some(state.vault.as_ref()).flatten();
            self.check_vault_baseline(baseline, local_vault.as_ref(), remote_vault.as_ref())?;
            if let Some(remote_vault) = remote_vault {
                let device_id = state.device_id.clone().unwrap_or_else(new_device_id);
                let save = local_vault
                    .as_ref()
                    .is_none_or(|vault| vault.fingerprint != remote_vault.fingerprint);
                return self.apply_remote_vault(
                    &client.file_url(),
                    &local,
                    &remote,
                    document,
                    &remote_vault.content,
                    remote_vault.cipher.vault_id(),
                    save,
                    &source,
                    &device_id,
                    baseline.is_none(),
                    "sync-use-remote",
                );
            }
            // No vault in the cloud and none here: the plain document applies.
        }
        if config.sync_secrets && local_vault.is_none() {
            self.sync_passphrase()?;
        }
        self.apply_remote(
            config,
            &client.file_url(),
            &local,
            &remote,
            "sync-use-remote",
        )
    }

    fn use_local(&self, config: &SyncConfig, client: &WebDavClient) -> Result<Step, SyncError> {
        let local = self.data.load().map_err(SyncError::Local)?;
        let remote = self.fetch(config, client)?;
        let remote_snapshot = match &remote {
            Some(remote) => Some(
                SnapshotStore::at(self.data.root())
                    .create(
                        "sync-use-local",
                        now_unix(),
                        &[(REMOTE_FILE_NAME, remote.bytes.as_slice())],
                    )
                    .map_err(SyncError::Local)?,
            ),
            None => None,
        };
        let local_vault = if config.sync_secrets {
            self.open_local_vault()?
        } else {
            None
        };
        let cloud_has_vault = remote
            .as_ref()
            .and_then(|remote| remote.document.as_ref().ok())
            .is_some_and(ShelluneData::has_vault);
        if config.sync_secrets && (local_vault.is_some() || cloud_has_vault) {
            // The vault travels: "use local" must carry this machine's vault —
            // or the choice would delete the only ciphertext in passing (R5).
            let Some(local_vault) = local_vault else {
                return Err(vault_gone());
            };
            let source = self.vault_source()?;
            let state = self.state();
            let device_id = state.device_id.clone().unwrap_or_else(new_device_id);
            let stamped = stamped_content(
                &local_vault.content,
                &device_id,
                top_record(&local_vault.content),
            )?;
            let write = WriteVault {
                content: stamped,
                vault_id: local_vault.vault_id.clone(),
                origin: VaultOrigin::LocalOnly,
            };
            let step = self.upload_document(
                config,
                client,
                &client.file_url(),
                &local,
                None,
                remote.as_ref(),
                DocumentVault::Write {
                    write: &write,
                    source: &source,
                    device_id: &device_id,
                },
                "sync-use-local",
            )?;
            return Ok(attach_snapshot(step, remote_snapshot));
        }
        let mine = self.fingerprint(config, &local, None)?;
        let step = self.upload_to(
            config,
            client,
            &client.file_url(),
            &local,
            &mine,
            remote.as_ref(),
        )?;
        Ok(attach_snapshot(step, remote_snapshot))
    }

    fn fetch(
        &self,
        config: &SyncConfig,
        client: &WebDavClient,
    ) -> Result<Option<Remote>, SyncError> {
        let Some(download) = client.get()? else {
            return Ok(None);
        };
        let document = ShelluneData::from_json(&download.bytes);
        let (view_hash, core_hash) = match &document {
            Ok(document) => (
                Some(hash(
                    &document
                        .sync_view(self.platform)
                        .map_err(SyncError::RemoteDocument)?,
                )),
                Some(hash(
                    &document
                        .core_view(self.platform)
                        .map_err(SyncError::RemoteDocument)?,
                )),
            ),
            Err(_) => (None, None),
        };
        let mark = remote_mark(
            view_hash.as_deref().unwrap_or_default(),
            &download.bytes,
            config.sync_secrets,
        );
        Ok(Some(Remote {
            etag: download.etag,
            bytes: download.bytes,
            document,
            view_hash,
            core_hash,
            mark,
        }))
    }

    fn sync_passphrase(&self) -> Result<SecretString, SyncError> {
        self.passphrase()
            .map_err(SyncError::Credentials)?
            .filter(|passphrase| !passphrase.expose().trim().is_empty())
            .ok_or(SyncError::PassphraseMissing)
    }

    /// Writes the local data to the cloud, keeping the other platforms' sections
    /// of `base`.
    fn upload_to(
        &self,
        config: &SyncConfig,
        client: &WebDavClient,
        remote_id: &str,
        local: &LocalState,
        mine: &Fingerprint,
        base: Option<&Remote>,
    ) -> Result<Step, SyncError> {
        let mut options = ExportOptions::new(self.platform);
        options.kdf = self.kdf;
        if config.sync_secrets {
            options = options.with_passphrase(self.sync_passphrase()?);
        }
        // A cloud document that carries a vault keeps it when this machine does
        // not sync the vault: dropping the block would delete the only
        // ciphertext without the user's explicit choice (R5).
        let carry = base
            .and_then(|remote| remote.document.as_ref().ok())
            .and_then(|document| document.vault().cloned());
        if let Some(cipher) = carry {
            options = options.with_vault(cipher);
        }
        let base_document = base.and_then(|remote| remote.document.as_ref().ok());
        let export = ShelluneData::export(local, &options, &*self.secrets, base_document)
            .map_err(SyncError::Local)?;
        let bytes = export.document.to_json().map_err(SyncError::Local)?;
        let Some(etag) = put(client, base, &bytes)? else {
            return Ok(Step::Retry);
        };
        let seen = self
            .seen_upload(config, &export.document, etag, &bytes)
            .map_err(SyncError::Local)?;
        self.remember_vault(remote_id, &seen, mine, None)?;
        Ok(Step::Done(SyncOutcome::Uploaded(UploadReport {
            created: base.is_none(),
            secrets_included: export.secrets_included,
            secrets_unreadable: export.secrets_unreadable,
            secrets_missing: export.secrets_missing,
            remote_snapshot: None,
        })))
    }

    /// Applies the cloud document over the local data (a replacement).
    fn apply_remote(
        &self,
        config: &SyncConfig,
        remote_id: &str,
        local: &LocalState,
        remote: &Remote,
        reason: &str,
    ) -> Result<Step, SyncError> {
        let document = remote
            .document
            .as_ref()
            .map_err(|error| SyncError::RemoteDocument(error.clone()))?;
        let bundle = if config.sync_secrets && document.has_secrets() {
            Some(
                document
                    .open_secrets(&self.sync_passphrase()?)
                    .map_err(SyncError::RemoteDocument)?,
            )
        } else {
            None
        };
        let plan = document
            .plan(local, self.platform, ApplyMode::Replace, bundle.as_ref())
            .map_err(SyncError::RemoteDocument)?;
        let applied = match plan.apply(&self.data, &*self.secrets, reason) {
            Err(PortableError::Changed) => return Ok(Step::Retry),
            other => other.map_err(SyncError::Local)?,
        };
        let mine = self.fingerprint(config, &applied.state, None)?;
        self.remember_vault(remote_id, &remote.seen(), &mine, None)?;
        Ok(Step::Done(SyncOutcome::Pulled(Box::new(pull_report(
            &plan,
            applied,
            document.has_secrets() && !config.sync_secrets,
            None,
        )))))
    }

    /// What the cloud holds after this machine wrote `document` as `bytes`.
    fn seen_upload(
        &self,
        config: &SyncConfig,
        document: &ShelluneData,
        etag: Option<String>,
        bytes: &[u8],
    ) -> Result<Seen, PortableError> {
        let view = hash(&document.sync_view(self.platform)?);
        Ok(Seen {
            etag,
            mark: remote_mark(&view, bytes, config.sync_secrets),
            core_hash: Some(hash(&document.core_view(self.platform)?)),
        })
    }

    /// Records what both sides hold after a successful sync. `record` is the
    /// vault half when one travelled: the baseline both sides now share, the
    /// stamped global counter and the device identity that wrote it. Without
    /// one, the vault baseline is cleared — the arrangement has no vault in it.
    fn remember_vault(
        &self,
        remote_id: &str,
        seen: &Seen,
        mine: &Fingerprint,
        record: Option<VaultRemember<'_>>,
    ) -> Result<(), SyncError> {
        let store = SyncStateStore::at(self.data.root());
        let mut state = store.load();
        state.remote = Some(remote_id.to_owned());
        state.last_remote_etag.clone_from(&seen.etag);
        state.last_remote_hash = Some(seen.mark.clone());
        state.last_remote_core_hash.clone_from(&seen.core_hash);
        state.last_synced_hash = Some(mine.view.clone());
        state.last_synced_core_hash = Some(mine.core.clone());
        match record {
            Some(record) => {
                state.device_id = Some(record.device_id.to_owned());
                // The version 1 sealed-password block never travels beside a
                // vault; what it held is the vault's business now.
                state.last_secrets_hash = None;
                state.last_vault_hash = Some(vault_fingerprint(&record.content));
                state.last_remote_vault_hash = Some(
                    record
                        .remote_fingerprint
                        .unwrap_or_else(|| vault_fingerprint(&record.content)),
                );
                state.vault = Some(baseline_of(&record.content, &record.vault_id));
            }
            None => {
                state.last_secrets_hash.clone_from(&mine.secrets);
                state.last_vault_hash.clone_from(&mine.vault);
                state.vault = None;
            }
        }
        store.save(&state)?;
        Ok(())
    }

    /// The fingerprint of the local document: its view and the same view
    /// without the recent connections.
    fn document_fingerprint(&self, local: &LocalState) -> Result<Fingerprint, SyncError> {
        let export = ShelluneData::export(
            local,
            &ExportOptions::new(self.platform),
            &*self.secrets,
            None,
        )
        .map_err(SyncError::Local)?;
        let view = hash(
            &export
                .document
                .sync_view(self.platform)
                .map_err(SyncError::Local)?,
        );
        let core = hash(
            &export
                .document
                .core_view(self.platform)
                .map_err(SyncError::Local)?,
        );
        Ok(Fingerprint {
            view,
            core,
            secrets: None,
            vault: None,
        })
    }

    /// The keyed fingerprint of the saved passwords, as the version 1 sealed
    /// block arrangement compares them.
    fn legacy_secrets_digest(&self, passphrase: &SecretString, local: &LocalState) -> String {
        let saved: Vec<(HostId, SecretString)> = local
            .hosts
            .records()
            .iter()
            .filter(|record| record.remembers_secret())
            .filter_map(|record| match self.secrets.load(record.id()) {
                Ok(Some(secret)) => Some((record.id(), secret)),
                _ => None,
            })
            .collect();
        secrets_digest(passphrase, saved.iter().map(|(id, secret)| (*id, secret)))
    }

    /// The fingerprint of the local data as this platform syncs it: the view of
    /// the document it would upload, the public fingerprint of its vault when
    /// the user asked for the vault to travel, and — when the version 1 sealed
    /// passwords are the arrangement — a fingerprint of them keyed by the sync
    /// passphrase (so the state file holds nothing a password could be guessed
    /// from without it).
    ///
    /// A machine without a passphrase computes no password fingerprint and
    /// fails here for no one: whether the sealed-password arrangement is even
    /// in play is only known once the cloud document is read, and the vault
    /// arrangement may need no passphrase at all (a WebDAV password travels
    /// instead).
    fn fingerprint(
        &self,
        config: &SyncConfig,
        local: &LocalState,
        local_vault: Option<&LocalVault>,
    ) -> Result<Fingerprint, SyncError> {
        let mut mine = self.document_fingerprint(local)?;
        mine.vault = config
            .sync_secrets
            .then(|| local_vault.map_or_else(|| hash(NO_VAULT), |vault| vault.fingerprint.clone()));
        if config.sync_secrets
            && local_vault.is_none()
            && let Some(passphrase) = self
                .passphrase()
                .map_err(SyncError::Credentials)?
                .filter(|passphrase| !passphrase.expose().trim().is_empty())
        {
            mine.secrets = Some(self.legacy_secrets_digest(&passphrase, local));
        }
        Ok(mine)
    }
}

/// What a written document carries as its vault block, and what happens to
/// this machine's own vault file.
enum DocumentVault<'a> {
    /// No vault travels (the legacy arrangement); a cloud block is carried
    /// untouched when the machine does not sync the vault.
    None,
    /// The cloud's block, carried unchanged; both sides hold `settled`.
    Carry {
        cipher: &'a VaultCipher,
        settled: &'a SettledVault,
        device_id: &'a str,
    },
    /// A merged content, sealed for the travel, applied here and saved as this
    /// machine's vault with its own local wrap.
    Write {
        write: &'a WriteVault,
        source: &'a VaultSource,
        device_id: &'a str,
    },
}

/// What a successful vault-carrying sync records in the state.
struct VaultRemember<'a> {
    device_id: &'a str,
    vault_id: String,
    content: VaultContent,
    /// The fingerprint of the vault the cloud ends up holding, when that is not
    /// simply the content itself.
    remote_fingerprint: Option<String>,
}

/// Whether each side changed more than its recent connections, for the vault
/// arrangement (no sealed-password block to open): `None` when the state
/// predates the fingerprints without recents.
fn check_core_docs(state: &SyncState, mine: &Fingerprint, remote: &Remote) -> Option<CoreChanges> {
    let (Some(baseline), Some(remote_baseline)) = (
        state.last_synced_core_hash.as_deref(),
        state.last_remote_core_hash.as_deref(),
    ) else {
        return None;
    };
    let remote_core = remote.core_hash.as_deref() != Some(remote_baseline);
    let same_core = remote.core_hash.as_deref() == Some(mine.core.as_str());
    Some(if same_core {
        CoreChanges {
            local: false,
            remote: false,
        }
    } else {
        CoreChanges {
            local: mine.core != baseline,
            remote: remote_core,
        }
    })
}

/// The refusal for a cloud that carried a vault and no longer does (R5, R6):
/// neither applied nor overwritten until the user decides.
fn vault_gone() -> SyncError {
    SyncError::Vault(VaultStoreError::Invalid {
        reason: "the cloud document no longer carries the encrypted vault this machine synced; \
                 refusing to apply or overwrite it"
            .to_owned(),
    })
}

/// Puts a kept copy of the replaced cloud file into an upload report.
fn attach_snapshot(step: Step, remote_snapshot: Option<PathBuf>) -> Step {
    match step {
        Step::Done(SyncOutcome::Uploaded(mut report)) => {
            report.remote_snapshot = remote_snapshot;
            Step::Done(SyncOutcome::Uploaded(report))
        }
        other => other,
    }
}

/// Writes `bytes` over `base` (If-Match its tag), or as a new file without one.
/// `None` when the server refused because the file changed meanwhile.
fn put(
    client: &WebDavClient,
    base: Option<&Remote>,
    bytes: &[u8],
) -> Result<Option<Option<String>>, SyncError> {
    let precondition = match base {
        None => Precondition::IfAbsent,
        Some(remote) => match &remote.etag {
            Some(etag) if is_strong_etag(etag) => Precondition::IfMatch(etag.clone()),
            // Without a usable tag the write cannot be made conditional.
            _ => Precondition::None,
        },
    };
    match client.put(bytes, &precondition) {
        Err(WebDavError::Conflict) => Ok(None),
        other => Ok(Some(other?)),
    }
}

/// What a pull or a merge applied, for the interface.
fn pull_report(
    plan: &ApplyPlan,
    applied: ApplyOutcome,
    secrets_ignored: bool,
    vault_restored: Option<VaultRestored>,
) -> PullReport {
    PullReport {
        vault_restored,
        snapshot: applied.snapshot,
        source_platform: plan.source_platform().to_owned(),
        source_app_version: plan.source_app_version().to_owned(),
        created_unix: plan.created_unix(),
        hosts: plan.hosts(),
        groups: plan.groups(),
        forwards: plan.forwards(),
        snippets: plan.snippets(),
        known_hosts: plan.known_hosts(),
        preferences_changed: plan.preferences_changed(),
        keymap_changed: plan.keymap_changed(),
        fallbacks: plan.fallbacks().to_vec(),
        trust_conflicts: plan.trust_conflicts().to_vec(),
        secrets_ignored,
        secrets_saved: applied.secrets_saved,
        secrets_to_forget: plan.secrets_to_forget().to_vec(),
        secrets_forgotten: applied.secrets_forgotten,
        secrets_not_forgotten: applied.secrets_not_forgotten,
        state: applied.state,
    }
}

/// A fingerprint of saved passwords keyed by the sync passphrase, so the state
/// file holds nothing a password could be guessed from without it. The same
/// for this machine's credential store and for the cloud's opened block when
/// both hold the same passwords for the same records in the same order.
fn secrets_digest<'a>(
    passphrase: &SecretString,
    saved: impl IntoIterator<Item = (HostId, &'a SecretString)>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(passphrase.expose().as_bytes());
    digest.update([0]);
    for (id, secret) in saved {
        digest.update(id.to_string().as_bytes());
        digest.update([0]);
        digest.update(secret.expose().as_bytes());
        digest.update([0]);
    }
    hex(&digest.finalize())
}

/// What this machine holds, as sync compares it.
struct Fingerprint {
    /// The fingerprint of the document view.
    view: String,
    /// The fingerprint of the view without the recent connections.
    core: String,
    /// The keyed fingerprint of the saved passwords, when they are synced.
    secrets: Option<String>,
    /// The fingerprint of the public part of this machine's vault, when the
    /// vault travels (`None` while it does not, `Some` of the content — or of
    /// nothing, for a machine with no vault file).
    vault: Option<String>,
}

fn set_app_secret(
    store: &dyn SecretStore,
    which: AppSecret,
    secret: Option<&SecretString>,
) -> Result<(), SecretError> {
    match secret {
        Some(secret) if !secret.expose().is_empty() => store.save_app(which, secret),
        _ => store.forget_app(which),
    }
}

/// The view fingerprint, followed by a fingerprint of the sealed password block
/// when passwords are synced here (a new password changes only the block), and
/// always by one of the encrypted vault block when the document carries one
/// (even a machine that does not sync the vault must notice another device
/// changing it, so it never writes a stale ciphertext over a newer one).
fn remote_mark(view_hash: &str, document: &[u8], with_secrets: bool) -> String {
    let Some(value) = serde_json::from_slice::<serde_json::Value>(document).ok() else {
        return view_hash.to_owned();
    };
    let mut mark = view_hash.to_owned();
    if with_secrets && let Some(secrets) = value.get("secrets") {
        mark.push('+');
        mark.push_str(&hash(secrets.to_string().as_bytes()));
    }
    if let Some(vault) = value.get("vault") {
        mark.push('+');
        mark.push_str(&hash(vault.to_string().as_bytes()));
    }
    mark
}

/// Whether a document holds no record of any kind (preferences do not count).
fn document_is_empty(document: &ShelluneData) -> bool {
    let shared = document.shared();
    shared.hosts().connection_count() == 0
        && shared.groups().is_empty()
        && shared.forwards().rules().is_empty()
        && shared.snippets().is_empty()
        && shared.known_hosts().is_empty()
}

fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

// ---------------------------------------------------------------------------
// The encrypted vault on sync (R5, R6, R8; ADR-2, ADR-5, ADR-6)
// ---------------------------------------------------------------------------

/// The fingerprint a machine without a vault file has, so "a vault appeared"
/// is a change the state can name.
const NO_VAULT: &[u8] = b"no vault";

/// Which secret protects the travelling vault (R5, ADR-2): the WebDAV account
/// password when there is one, otherwise the sync passphrase an account
/// without a password must have set before the vault may travel.
#[derive(Clone)]
enum VaultSource {
    /// The WebDAV account password, reused as the unseal secret.
    WebDavPassword(SecretString),
    /// The sync passphrase of a passwordless account.
    SyncPassphrase(SecretString),
}

impl VaultSource {
    fn key_source(&self) -> shellune_storage::KeySource {
        match self {
            Self::WebDavPassword(secret) => shellune_storage::KeySource::Passphrase {
                purpose: PassphrasePurpose::WebDavPassword,
                value: secret.clone(),
            },
            Self::SyncPassphrase(secret) => shellune_storage::KeySource::Passphrase {
                purpose: PassphrasePurpose::SyncPassphrase,
                value: secret.clone(),
            },
        }
    }

    fn unseal_source(&self) -> UnsealSource {
        match self {
            Self::WebDavPassword(secret) => UnsealSource::WebDavPassword(secret.clone()),
            Self::SyncPassphrase(secret) => UnsealSource::SyncPassphrase(secret.clone()),
        }
    }
}

/// This machine's vault as one sync sees it: opened, identified by its cipher
/// and fingerprinted by its public part.
struct LocalVault {
    content: VaultContent,
    vault_id: String,
    fingerprint: String,
}

/// The cloud document's vault as one sync sees it.
struct RemoteVault {
    content: VaultContent,
    /// The block exactly as the cloud holds it, so a vault neither side changed
    /// travels without a pointless re-seal.
    cipher: VaultCipher,
    fingerprint: String,
}

/// Which side a merged vault's changes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VaultOrigin {
    /// Every change is this machine's; the cloud held the baseline.
    LocalOnly,
    /// Every change is the cloud's; this machine adopts it.
    RemoteOnly,
    /// Both sides changed, on different records; the merge holds both.
    Combined,
}

/// The plan the vault side of one sync follows.
enum VaultPlan {
    /// No vault travels on either side.
    Absent,
    /// Both sides already hold this same content; nothing is written. What
    /// follows is what the new baseline records.
    Unchanged(Box<SettledVault>),
    /// This content is applied here and written to the cloud.
    Write(Box<WriteVault>),
}

/// A vault both sides already hold.
struct SettledVault {
    content: VaultContent,
    vault_id: String,
}

/// A vault to apply and write.
struct WriteVault {
    /// The merged records and trust, references unset (they are derived from
    /// the host book the document apply ends with) and already stamped with
    /// the global version of this write (ADR-5: the merge's counter is both
    /// sides' maximum plus one, written by the device that merges).
    content: VaultContent,
    vault_id: String,
    origin: VaultOrigin,
}

/// One record as the three-way decision compares it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RecordState<'a> {
    counter: u64,
    device_id: &'a str,
    deleted: bool,
}

impl<'a> RecordState<'a> {
    fn from_base(base: &'a VaultRecordBase) -> Self {
        Self {
            counter: base.counter,
            device_id: base.device_id.as_str(),
            deleted: base.deleted,
        }
    }
}

/// The view of a record the merge needs, over both record kinds.
trait RecordView {
    fn state(&self) -> RecordState<'_>;
    fn meta_name(&self) -> &str;
    /// Whether two records of one version and liveness also hold the same
    /// metadata and the same secret: a changed body under an unchanged version
    /// is a conflict, not a fact to average over.
    fn same_body_as(&self, other: &Self) -> bool;
}

impl RecordView for VaultKeyRecord {
    fn state(&self) -> RecordState<'_> {
        RecordState {
            counter: self.meta().version().counter(),
            device_id: self.meta().version().device_id(),
            deleted: !self.meta().is_live(),
        }
    }
    fn meta_name(&self) -> &str {
        self.meta().name()
    }
    fn same_body_as(&self, other: &Self) -> bool {
        self.meta() == other.meta()
            && self.private_key().map(SecretString::expose)
                == other.private_key().map(SecretString::expose)
    }
}

impl RecordView for VaultCredentialRecord {
    fn state(&self) -> RecordState<'_> {
        RecordState {
            counter: self.meta().version().counter(),
            device_id: self.meta().version().device_id(),
            deleted: !self.meta().is_live(),
        }
    }
    fn meta_name(&self) -> &str {
        self.meta().name()
    }
    fn same_body_as(&self, other: &Self) -> bool {
        self.meta() == other.meta()
            && self.secret().map(SecretString::expose) == other.secret().map(SecretString::expose)
    }
}

/// The fingerprint of the public part of a vault: the metadata of every record
/// (version and tombstone included) and the accepted server identities. No
/// secret, and no host-reference mirror: references are a function of the host
/// book, which [`VaultContent::validate_for`] checks as a whole at every write.
fn vault_fingerprint(content: &VaultContent) -> String {
    // The metadata serializes with its name, timestamps, version and possible
    // tombstone; the canonical form of the whole set is the fingerprint.
    let keys: Vec<(String, serde_json::Value)> = content
        .keys()
        .map(|r| {
            (
                r.meta().id().to_string(),
                serde_json::to_value(r.meta()).unwrap_or_default(),
            )
        })
        .collect();
    let credentials: Vec<(String, serde_json::Value)> = content
        .credentials()
        .map(|r| {
            (
                r.meta().id().to_string(),
                serde_json::to_value(r.meta()).unwrap_or_default(),
            )
        })
        .collect();
    let value = serde_json::json!({
        "keys": keys,
        "credentials": credentials,
        "trust": serde_json::to_value(content.trust()).unwrap_or_default(),
    });
    hash(&serde_json::to_vec(&value).unwrap_or_default())
}

/// The global version of a vault: the highest record counter it holds. Every
/// write a sync makes stamps this one higher (ADR-5), which is what makes an
/// abnormally rolled-back cloud recognisable by a device that has a baseline.
fn vault_global(content: &VaultContent) -> u64 {
    content
        .keys()
        .map(|record| record.meta().version().counter())
        .chain(
            content
                .credentials()
                .map(|record| record.meta().version().counter()),
        )
        .max()
        .unwrap_or(0)
}

/// How many live records a vault holds.
fn vault_live_records(content: &VaultContent) -> usize {
    content.keys().filter(|r| r.meta().is_live()).count()
        + content.credentials().filter(|r| r.meta().is_live()).count()
}

/// The baseline of a vault state, for the state file. Public facts only.
fn baseline_of(content: &VaultContent, vault_id: &str) -> VaultBaseline {
    let base = |meta: &dyn RecordMetaView| VaultRecordBase {
        counter: meta.version_counter(),
        device_id: meta.version_device_id().to_owned(),
        deleted: !meta.is_live(),
    };
    VaultBaseline {
        vault_id: vault_id.to_owned(),
        global_counter: vault_global(content),
        keys: content
            .keys()
            .map(|record| (record.meta().id(), base(&KeyMeta(record.meta()))))
            .collect(),
        credentials: content
            .credentials()
            .map(|record| (record.meta().id(), base(&CredentialMetaView(record.meta()))))
            .collect(),
        trust: content
            .trust()
            .entries()
            .iter()
            .map(|entry| {
                (
                    format!("{}:{}", entry.host(), entry.port()),
                    entry.fingerprint().to_owned(),
                )
            })
            .collect(),
    }
}

/// The metadata surface the baseline builder reads, over both kinds.
trait RecordMetaView {
    fn version_counter(&self) -> u64;
    fn version_device_id(&self) -> &str;
    fn is_live(&self) -> bool;
}

/// One kind's adapter, so the closure above stays one.
struct KeyMeta<'a>(&'a ManagedKeyMeta);

impl RecordMetaView for KeyMeta<'_> {
    fn version_counter(&self) -> u64 {
        self.0.version().counter()
    }
    fn version_device_id(&self) -> &str {
        self.0.version().device_id()
    }
    fn is_live(&self) -> bool {
        self.0.is_live()
    }
}

/// The credential kind's adapter.
struct CredentialMetaView<'a>(&'a CredentialMeta);

impl RecordMetaView for CredentialMetaView<'_> {
    fn version_counter(&self) -> u64 {
        self.0.version().counter()
    }
    fn version_device_id(&self) -> &str {
        self.0.version().device_id()
    }
    fn is_live(&self) -> bool {
        self.0.is_live()
    }
}

/// Which side a settled three-way decision takes its value from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    Local,
    Remote,
}

/// The three-way decision for one value against the common baseline (ADR-5):
/// which side to take, or `None` for a conflict only the user can settle.
///
/// The values are compared as `Option`s: a record's version and liveness, a
/// trust entry's fingerprint, where `None` means "not there". A first sync
/// without a baseline compares against `None`, which makes records only one
/// side holds that side's and equal values adopted.
fn three_way<T: PartialEq + Copy>(
    base: Option<T>,
    local: Option<T>,
    remote: Option<T>,
) -> Option<Which> {
    let local_changed = local != base;
    let remote_changed = remote != base;
    match (local_changed, remote_changed) {
        // Both hold the baseline (or neither side has the value at all).
        (false, false) => Some(Which::Local),
        (true, false) => Some(Which::Local),
        (false, true) => Some(Which::Remote),
        (true, true) if local == remote => Some(Which::Local),
        (true, true) => None,
    }
}

/// What the vault merge decided.
struct VaultMerge {
    plan: VaultPlan,
    conflicts: Vec<VaultConflict>,
}

/// Merges the two vaults record by record and trust entry by trust entry
/// against the common baseline (ADR-5, ADR-8).
///
/// Only the baseline decides: a record only this machine changed is this
/// machine's, one only the cloud changed is the cloud's, and records neither
/// side touched stay. The same record changed differently — or deleted on one
/// side and changed on the other — and one server trusted with two different
/// identities are conflicts that carry both sides' public facts for the user;
/// a choice in `choices` settles one, and any conflict left unsettled stops
/// the whole vault: no partial vault is ever applied. A cloud identity is
/// never taken automatically (R8), and "the cloud is newer" authorises
/// nothing.
///
/// The merged global version is both sides' maximum plus one, stamped by the
/// merging device, except for an adoption (the cloud's content as it stands).
fn merge_vault(
    local: Option<&LocalVault>,
    remote: Option<&RemoteVault>,
    baseline: Option<&VaultBaseline>,
    choices: &[VaultChoice],
    device_id: &str,
    document: &ShelluneData,
) -> Result<VaultMerge, SyncError> {
    if local.is_none() && remote.is_none() {
        return Ok(VaultMerge {
            plan: VaultPlan::Absent,
            conflicts: Vec::new(),
        });
    }
    let local_content = local.map(|vault| &vault.content);
    let remote_content = remote.map(|vault| &vault.content);
    let mut conflicts = Vec::new();
    let mut content = VaultContent::new();

    let key_outcomes = key_decisions(
        local_content,
        remote_content,
        baseline,
        choices,
        &mut conflicts,
    );
    for (_id, record, _changed) in &key_outcomes {
        if let Some(record) = record {
            content
                .insert_key((*record).clone())
                .map_err(SyncError::Vault)?;
        }
    }
    let credential_outcomes = credential_decisions(
        local_content,
        remote_content,
        baseline,
        choices,
        &mut conflicts,
    );
    for (_id, record, _changed) in &credential_outcomes {
        if let Some(record) = record {
            content
                .insert_credential((*record).clone())
                .map_err(SyncError::Vault)?;
        }
    }
    content.set_trust(merged_trust(
        local_content,
        remote_content,
        baseline,
        choices,
        document,
        &mut conflicts,
    )?);

    let fingerprint = vault_fingerprint(&content);
    let local_fp = local.map(|vault| vault.fingerprint.as_str());
    let remote_fp = remote.map(|vault| vault.fingerprint.as_str());
    // The identity a merged vault keeps: this machine's when it has a file of
    // its own (its wraps stay bound to it), otherwise the cloud's.
    let vault_id = local
        .map(|vault| vault.vault_id.clone())
        .or_else(|| remote.map(|vault| vault.cipher.vault_id().to_owned()))
        .unwrap_or_default();
    let plan = if local_fp == Some(fingerprint.as_str()) && remote_fp == Some(fingerprint.as_str())
    {
        VaultPlan::Unchanged(Box::new(SettledVault { vault_id, content }))
    } else {
        let origin = if local_fp == Some(fingerprint.as_str()) {
            VaultOrigin::LocalOnly
        } else if remote_fp == Some(fingerprint.as_str()) {
            VaultOrigin::RemoteOnly
        } else {
            VaultOrigin::Combined
        };
        // An adoption takes the cloud's version as it stands; everything this
        // device writes carries the new stamped global version. The stamp goes
        // on a record this merge changed — stamping an untouched record would
        // rewrite its version and read as a conflicting edit on the other side.
        let content = if origin == VaultOrigin::RemoteOnly {
            content
        } else {
            let stamp = key_outcomes
                .iter()
                .find(|(_id, _record, changed)| *changed)
                .map(|(id, _record, _changed)| StampRecord::Key(*id))
                .or_else(|| {
                    credential_outcomes
                        .iter()
                        .find(|(_id, _record, changed)| *changed)
                        .map(|(id, _record, _changed)| StampRecord::Credential(*id))
                })
                .unwrap_or_else(|| top_record(&content));
            stamped_content(&content, device_id, stamp)?
        };
        VaultPlan::Write(Box::new(WriteVault {
            content,
            vault_id,
            origin,
        }))
    };
    Ok(VaultMerge { plan, conflicts })
}

/// The key records of the merged vault, in identifier order: the record, and
/// whether the merge changed it.
fn key_decisions<'a>(
    local: Option<&'a VaultContent>,
    remote: Option<&'a VaultContent>,
    baseline: Option<&VaultBaseline>,
    choices: &[VaultChoice],
    conflicts: &mut Vec<VaultConflict>,
) -> Vec<(KeyId, Option<&'a VaultKeyRecord>, bool)> {
    let mut ids: Vec<KeyId> = local
        .map(|content| content.keys().map(|record| record.meta().id()).collect())
        .unwrap_or_default();
    if let Some(content) = remote {
        ids.extend(content.keys().map(|record| record.meta().id()));
    }
    make_unique(&mut ids);
    ids.into_iter()
        .map(|id| {
            let (record, changed) = record_decision(
                id,
                local.and_then(|content| content.key(id)),
                remote.and_then(|content| content.key(id)),
                baseline.and_then(|base| base.keys.get(&id)),
                VaultRecordKind::ManagedKey,
                choices,
                conflicts,
            );
            (id, record, changed)
        })
        .collect()
}

/// The credential records of the merged vault, in identifier order: the
/// record, and whether the merge changed it.
fn credential_decisions<'a>(
    local: Option<&'a VaultContent>,
    remote: Option<&'a VaultContent>,
    baseline: Option<&VaultBaseline>,
    choices: &[VaultChoice],
    conflicts: &mut Vec<VaultConflict>,
) -> Vec<(CredentialId, Option<&'a VaultCredentialRecord>, bool)> {
    let mut ids: Vec<CredentialId> = local
        .map(|content| {
            content
                .credentials()
                .map(|record| record.meta().id())
                .collect()
        })
        .unwrap_or_default();
    if let Some(content) = remote {
        ids.extend(content.credentials().map(|record| record.meta().id()));
    }
    make_unique(&mut ids);
    ids.into_iter()
        .map(|id| {
            let (record, changed) = record_decision(
                id,
                local.and_then(|content| content.credential(id)),
                remote.and_then(|content| content.credential(id)),
                baseline.and_then(|base| base.credentials.get(&id)),
                VaultRecordKind::Credential,
                choices,
                conflicts,
            );
            (id, record, changed)
        })
        .collect()
}

/// Sorted, without duplicates.
fn make_unique<T: Ord>(values: &mut Vec<T>) {
    values.sort();
    values.dedup();
}

/// One record's three-way decision, conflict included (see [`merge_vault`]):
/// the record the merged vault holds, and whether this record is one the merge
/// changed (which is where the global version stamp belongs).
fn record_decision<'a, R: RecordView>(
    id: impl std::fmt::Display + Copy,
    local: Option<&'a R>,
    remote: Option<&'a R>,
    base: Option<&VaultRecordBase>,
    kind: VaultRecordKind,
    choices: &[VaultChoice],
    conflicts: &mut Vec<VaultConflict>,
) -> (Option<&'a R>, bool) {
    fn state_of<'r, R2: RecordView>(record: Option<&'r R2>) -> Option<RecordState<'r>> {
        record.map(|record| record.state())
    }
    let local_state = state_of(local);
    let remote_state = state_of(remote);
    let base_state = base.map(RecordState::from_base);
    let changed = local_state != base_state || remote_state != base_state;
    let chosen = |which: Which| match which {
        Which::Local => local,
        Which::Remote => remote,
    };
    let outcome = match three_way(base_state, local_state, remote_state) {
        Some(which) => {
            let record = chosen(which);
            // Versions and liveness agreeing does not make the records equal:
            // a changed body under an unchanged version is a conflict (the
            // version bump is the writer's duty), not a fact to pick between.
            if let (Some(local_record), Some(remote_record)) = (local, remote)
                && local_state == remote_state
                && !local_record.same_body_as(remote_record)
            {
                record_conflict(
                    id,
                    kind,
                    local,
                    remote,
                    local_state,
                    remote_state,
                    choices,
                    conflicts,
                )
            } else {
                record
            }
        }
        None => record_conflict(
            id,
            kind,
            local,
            remote,
            local_state,
            remote_state,
            choices,
            conflicts,
        ),
    };
    (outcome, changed)
}

/// Records one record's conflict — or settles it by a choice — and returns the
/// side the choice kept. An unsettled conflict contributes no record: nothing
/// of the vault is applied while any conflict stands.
#[allow(clippy::too_many_arguments)]
fn record_conflict<'a, R: RecordView>(
    id: impl std::fmt::Display + Copy,
    kind: VaultRecordKind,
    local: Option<&'a R>,
    remote: Option<&'a R>,
    local_state: Option<RecordState<'_>>,
    remote_state: Option<RecordState<'_>>,
    choices: &[VaultChoice],
    conflicts: &mut Vec<VaultConflict>,
) -> Option<&'a R> {
    let key = VaultChoiceKey::Record {
        kind,
        id: id.to_string(),
    };
    if let Some(choice) = choices.iter().find(|choice| choice.key == key) {
        return match choice.side {
            VaultChoiceSide::Local => local,
            VaultChoiceSide::Remote => remote,
        };
    }
    let side = |record: Option<&R>, state: Option<RecordState>| VaultRecordSide {
        name: record
            .map(|record| record.meta_name().to_owned())
            .unwrap_or_default(),
        deleted: !state.is_some_and(|state| !state.deleted),
        counter: state.map_or(0, |state| state.counter),
        device_id: state.map_or_else(String::new, |state| state.device_id.to_owned()),
    };
    conflicts.push(VaultConflict::Record {
        kind,
        id: id.to_string(),
        name: local
            .or(remote)
            .map(|record| record.meta_name().to_owned())
            .unwrap_or_default(),
        local: side(local, local_state),
        remote: side(remote, remote_state),
    });
    None
}

/// The merged accepted-server identities (ADR-8): the side that changed an
/// entry wins it, an entry only one side accepted arrives, both forgetting
/// keeps it forgotten, and one server trusted with two different identities —
/// or a deletion against a change — is a conflict carrying both complete
/// fingerprints, the cloud's platform and its version time. The cloud's
/// identity is never adopted without the user's choice.
fn merged_trust(
    local: Option<&VaultContent>,
    remote: Option<&VaultContent>,
    baseline: Option<&VaultBaseline>,
    choices: &[VaultChoice],
    document: &ShelluneData,
    conflicts: &mut Vec<VaultConflict>,
) -> Result<shellune_storage::HostTrust, SyncError> {
    use shellune_storage::{HostKey, HostTrust, TrustEntry};
    let invalid = |reason: String| SyncError::Vault(VaultStoreError::Invalid { reason });
    let mut trust = HostTrust::new();
    let mut keys: Vec<(String, u16)> = local
        .map(|content| {
            content
                .trust()
                .entries()
                .iter()
                .map(|entry| (entry.host().to_owned(), entry.port()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(content) = remote {
        keys.extend(
            content
                .trust()
                .entries()
                .iter()
                .map(|entry| (entry.host().to_owned(), entry.port())),
        );
    }
    make_unique(&mut keys);
    let accept = |trust: &mut HostTrust, entry: &TrustEntry| -> Result<(), SyncError> {
        trust
            .accept(
                entry.host(),
                entry.port(),
                &HostKey::new(
                    entry.algorithm().to_owned(),
                    entry.fingerprint().to_owned(),
                    entry.public_key().to_owned(),
                )
                .map_err(|error| invalid(error.to_string()))?,
                entry.accepted_unix(),
            )
            .map_err(|error| invalid(error.to_string()))
    };
    for (host, port) in keys {
        fn entry_of<'a>(
            content: &Option<&'a VaultContent>,
            host: &str,
            port: u16,
        ) -> Option<&'a shellune_storage::TrustEntry> {
            content.and_then(|content| content.trust().entry(host, port))
        }
        let local_entry = entry_of(&local, &host, port);
        let remote_entry = entry_of(&remote, &host, port);
        let fingerprint_of =
            |entry: Option<&TrustEntry>| entry.map(|entry| entry.fingerprint().to_owned());
        let base_fp = baseline
            .and_then(|base| base.trust.get(&format!("{host}:{port}")))
            .cloned();
        let local_fp = fingerprint_of(local_entry);
        let remote_fp = fingerprint_of(remote_entry);
        match three_way(
            base_fp.as_deref(),
            local_fp.as_deref(),
            remote_fp.as_deref(),
        ) {
            Some(which) => {
                let entry = match which {
                    Which::Local => local_entry,
                    Which::Remote => remote_entry,
                };
                // Neither side trusting the server — both forgot, or neither
                // ever did — writes no entry.
                if let Some(entry) = entry {
                    accept(&mut trust, entry)?;
                }
            }
            None => {
                let key = VaultChoiceKey::Trust {
                    host: host.clone(),
                    port,
                };
                match choices.iter().find(|choice| choice.key == key) {
                    Some(choice) => {
                        let entry = match choice.side {
                            VaultChoiceSide::Local => local_entry,
                            VaultChoiceSide::Remote => remote_entry,
                        };
                        // A choice for the side that deleted keeps the server
                        // untrusted: no entry is written.
                        if let Some(entry) = entry {
                            accept(&mut trust, entry)?;
                        }
                    }
                    None => {
                        let identity = |entry: Option<&TrustEntry>| {
                            entry.map(|entry| TrustIdentity {
                                algorithm: entry.algorithm().to_owned(),
                                fingerprint: entry.fingerprint().to_owned(),
                                accepted_unix: entry.accepted_unix(),
                            })
                        };
                        conflicts.push(VaultConflict::Trust {
                            host: host.clone(),
                            port,
                            local: identity(local_entry),
                            remote: identity(remote_entry),
                            remote_source_platform: document.source_platform().to_owned(),
                            remote_app_version: document.source_app_version().to_owned(),
                            remote_created_unix: document.created_unix(),
                        });
                    }
                }
            }
        }
    }
    Ok(trust)
}

/// Which record a write stamps its global version onto.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StampRecord {
    /// A managed key.
    Key(KeyId),
    /// A shared credential.
    Credential(CredentialId),
}

/// The record of the highest version counter, for a write that changed no
/// record (a trust entry or the reference mirror): the stamp still belongs on
/// some record, and the top one is the deterministic choice.
fn top_record(content: &VaultContent) -> StampRecord {
    let global = vault_global(content);
    for record in content.keys() {
        if record.meta().version().counter() == global {
            return StampRecord::Key(record.meta().id());
        }
    }
    for record in content.credentials() {
        if record.meta().version().counter() == global {
            return StampRecord::Credential(record.meta().id());
        }
    }
    // An empty vault has no record to stamp; `stamped_content` keeps it.
    StampRecord::Credential(CredentialId::new())
}

/// Rebuilds `content` with `stamp` raised to the new global version, written
/// by `device_id` (ADR-5: the merge's counter is both sides' maximum plus one,
/// and the device that writes it is named). An empty vault has nothing to
/// stamp; its version stays zero.
fn stamped_content(
    content: &VaultContent,
    device_id: &str,
    stamp: StampRecord,
) -> Result<VaultContent, SyncError> {
    let global = vault_global(content);
    if global == 0 {
        return Ok(content.clone());
    }
    let version = VaultRecordVersion::new(global + 1, device_id);
    let mut stamped = VaultContent::new();
    for record in content.keys() {
        let meta = record.meta();
        let meta = if stamp == StampRecord::Key(meta.id()) {
            with_version(meta, version.clone())?
        } else {
            meta.clone()
        };
        let body = record
            .private_key()
            .map(|secret| SecretString::new(secret.expose()))
            .unwrap_or_else(|| SecretString::new(""));
        stamped
            .insert_key(VaultKeyRecord::new(meta, body))
            .map_err(SyncError::Vault)?;
    }
    for record in content.credentials() {
        let meta = record.meta();
        let meta = if stamp == StampRecord::Credential(meta.id()) {
            with_version(meta, version.clone())?
        } else {
            meta.clone()
        };
        let body = record
            .secret()
            .map(|secret| SecretString::new(secret.expose()))
            .unwrap_or_else(|| SecretString::new(""));
        stamped
            .insert_credential(VaultCredentialRecord::new(meta, body))
            .map_err(SyncError::Vault)?;
    }
    stamped.set_references(content.references());
    stamped.set_trust(content.trust().clone());
    Ok(stamped)
}

/// The same metadata with another record version. The version field is
/// deliberately private in the core model — exactly so nobody bumps a version
/// in passing — so the change goes the one way the model offers: a round trip
/// that storage itself uses for tombstones.
fn with_version<T: Serialize + serde::de::DeserializeOwned>(
    meta: &T,
    version: VaultRecordVersion,
) -> Result<T, SyncError> {
    let encode = |error: serde_json::Error| {
        SyncError::Vault(VaultStoreError::Encode {
            reason: error.to_string(),
        })
    };
    let mut value = serde_json::to_value(meta).map_err(encode)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "version".to_owned(),
            serde_json::to_value(&version).map_err(encode)?,
        );
    }
    serde_json::from_value(value).map_err(|error| {
        SyncError::Vault(VaultStoreError::Corrupt {
            reason: error.to_string(),
        })
    })
}

/// A fresh device identity for the record versions this machine writes, kept
/// in the state from the first sync on.
fn new_device_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The SSH host records of a state: the hosts a vault's reference mirror is
/// the mirror of.
fn ssh_hosts(state: &LocalState) -> Vec<HostConfig> {
    state
        .hosts
        .records()
        .iter()
        .filter_map(|record| match record {
            shellune_core::ConnectionRecord::Ssh(host) => Some(host.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            first_sync: false,
            local_changed: false,
            remote_changed: false,
            local_empty: false,
            remote_empty: false,
            same_view: false,
            core: None,
        }
    }

    #[test]
    fn the_decision_table() {
        use Action::{Conflict, Nothing, Pull, Upload};
        use Direction::{Auto, Pull as AskPull, Upload as AskUpload};
        let cases = [
            (false, false, [Nothing, Nothing, Nothing]),
            (true, false, [Upload, Upload, Conflict]),
            (false, true, [Pull, Conflict, Pull]),
            (true, true, [Conflict, Conflict, Conflict]),
        ];
        for (local_changed, remote_changed, expected) in cases {
            for (direction, action) in [Auto, AskUpload, AskPull].into_iter().zip(expected) {
                let facts = Facts {
                    local_changed,
                    remote_changed,
                    ..facts()
                };
                assert_eq!(
                    decide(direction, facts),
                    action,
                    "{direction:?} {local_changed} {remote_changed}"
                );
            }
        }
        // Equal data is never a conflict.
        let same = Facts {
            local_changed: true,
            remote_changed: true,
            same_view: true,
            ..facts()
        };
        assert_eq!(decide(Auto, same), Nothing);
    }

    #[test]
    fn only_connecting_on_one_side_merges_in_automatic_sync() {
        use Action::{Conflict, Merge};
        let both = Facts {
            local_changed: true,
            remote_changed: true,
            ..facts()
        };
        let with = |local, remote| Facts {
            core: Some(CoreChanges { local, remote }),
            ..both
        };
        // Without the facts about recents (an older state, or passwords that do
        // not open) both changing stays a conflict.
        assert_eq!(decide(Direction::Auto, both), Conflict);
        assert_eq!(decide(Direction::Auto, with(false, true)), Merge);
        assert_eq!(decide(Direction::Auto, with(true, false)), Merge);
        assert_eq!(decide(Direction::Auto, with(false, false)), Merge);
        assert_eq!(decide(Direction::Auto, with(true, true)), Conflict);
        // "Upload now" and "pull" never merge.
        for direction in [Direction::Upload, Direction::Pull] {
            assert_eq!(decide(direction, with(false, false)), Conflict);
        }
        // A first sync never merges either.
        let first = Facts {
            first_sync: true,
            ..with(false, false)
        };
        assert_eq!(decide(Direction::Auto, first), Conflict);
    }

    #[test]
    fn the_vault_three_way_decision_table() {
        use crate::engine::Which;
        let base = Some(RecordState {
            counter: 2,
            device_id: "old",
            deleted: false,
        });
        let same = base;
        let local_edit = Some(RecordState {
            counter: 3,
            device_id: "here",
            deleted: false,
        });
        let remote_edit = Some(RecordState {
            counter: 4,
            device_id: "there",
            deleted: false,
        });
        let deleted = Some(RecordState {
            counter: 2,
            device_id: "old",
            deleted: true,
        });
        // Nothing changed anywhere.
        assert_eq!(three_way(base, same, same), Some(Which::Local));
        // One side changed; that side wins.
        assert_eq!(three_way(base, local_edit, same), Some(Which::Local));
        assert_eq!(three_way(base, same, remote_edit), Some(Which::Remote));
        // Both changed into the same result: adopted, no conflict.
        assert_eq!(three_way(base, local_edit, local_edit), Some(Which::Local));
        // Both changed differently — two edits, or a delete against an edit:
        // only the user can settle it (ADR-5).
        assert_eq!(three_way(base, local_edit, remote_edit), None);
        assert_eq!(three_way(base, deleted, remote_edit), None);
        assert_eq!(three_way(base, local_edit, deleted), None);
        // Both deleted: the same result.
        assert_eq!(three_way(base, deleted, deleted), Some(Which::Local));
        // Without a baseline (a first contact) a value only one side holds is
        // that side's, and equal values agree.
        assert_eq!(three_way(None, local_edit, None), Some(Which::Local));
        assert_eq!(three_way(None, None, remote_edit), Some(Which::Remote));
        assert_eq!(three_way(None, local_edit, local_edit), Some(Which::Local));
        assert_eq!(three_way(None, local_edit, remote_edit), None);
    }

    #[test]
    fn the_first_sync_rules() {
        use Action::{Conflict, Pull, Upload};
        let first = Facts {
            first_sync: true,
            local_changed: true,
            remote_changed: true,
            ..facts()
        };
        let empty_here = Facts {
            local_empty: true,
            ..first
        };
        let empty_there = Facts {
            remote_empty: true,
            ..first
        };
        assert_eq!(decide(Direction::Auto, empty_here), Pull);
        assert_eq!(decide(Direction::Auto, empty_there), Upload);
        assert_eq!(decide(Direction::Auto, first), Conflict);
        assert_eq!(decide(Direction::Upload, empty_there), Upload);
        assert_eq!(decide(Direction::Upload, empty_here), Conflict);
        assert_eq!(decide(Direction::Pull, empty_here), Pull);
        assert_eq!(decide(Direction::Pull, empty_there), Conflict);
    }
}
