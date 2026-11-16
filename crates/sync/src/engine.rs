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

use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use shellune_core::{HostId, SecretString};
use shellune_storage::{
    AppSecret, ApplyMode, ApplyOutcome, ApplyPlan, Change, ExportOptions, KdfParams, LocalData,
    LocalState, Platform, PlatformFallback, PortableError, SecretBundle, SecretError, SecretStore,
    ShelluneData, SnapshotStore, TrustConflict, merge_recent_connections,
};

use crate::config::{
    ConfigError, LastResult, SyncConfig, SyncConfigStore, SyncState, SyncStateStore,
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
    /// Saved passwords are synced, but no sync passphrase is saved on this machine.
    #[error("syncing saved passwords is on, but no sync passphrase is set")]
    PassphraseMissing,
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
        let remote_id = client.file_url();
        let state = self.state();
        let known = state.knows(&remote_id);
        let local = self.data.load().map_err(SyncError::Local)?;
        let mine = self.fingerprint(config, &local)?;
        // Passwords were just switched on here: the cloud may already carry them
        // (from another machine), and this machine's own set is not newer.
        let adopting_secrets = known && config.sync_secrets && state.last_secrets_hash.is_none();
        let secrets_changed = known && !adopting_secrets && state.last_secrets_hash != mine.secrets;
        let local_changed = !known
            || state.last_synced_hash.as_deref() != Some(mine.view.as_str())
            || secrets_changed;

        let Some(file) = client.stat()? else {
            return match direction {
                Direction::Pull => Err(SyncError::NothingInCloud),
                // Nothing on either side: nothing to create yet.
                Direction::Auto if local.is_empty() => Ok(Step::Done(SyncOutcome::UpToDate)),
                Direction::Auto | Direction::Upload => {
                    self.upload_to(config, client, &remote_id, &local, &mine, None)
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
            })))
        };
        match decide(direction, facts) {
            Action::Nothing => {
                self.remember(&remote_id, &remote.seen(), &mine)?;
                Ok(Step::Done(SyncOutcome::UpToDate))
            }
            Action::Upload => {
                self.upload_to(config, client, &remote_id, &local, &mine, Some(&remote))
            }
            Action::Pull => self.apply_remote(config, &remote_id, &local, &remote, "sync-pull"),
            Action::Merge => match check {
                Some(check) => self.merge(config, client, &remote_id, &local, &remote, check),
                None => conflict(),
            },
            Action::Conflict => conflict(),
        }
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
    /// decides again too.
    fn merge(
        &self,
        config: &SyncConfig,
        client: &WebDavClient,
        remote_id: &str,
        local: &LocalState,
        remote: &Remote,
        check: CoreCheck,
    ) -> Result<Step, SyncError> {
        let document = remote
            .document
            .as_ref()
            .map_err(|error| SyncError::RemoteDocument(error.clone()))?;
        let recents =
            merge_recent_connections(local.hosts.recents(), document.shared().hosts().recents());
        let from_remote = check.changes.remote || !check.changes.local;
        let mut options = ExportOptions::new(self.platform);
        options.kdf = self.kdf;
        let (plan, upload) = if from_remote {
            // When the cloud's recents are the later ones it already holds the
            // result, in its own order.
            let theirs = merge_recent_connections(document.shared().hosts().recents(), &[]);
            let mut merged = if theirs == recents {
                None
            } else {
                // The cloud's document with the merged recents; its sealed
                // passwords and the other platforms' sections travel unchanged.
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
            if config.sync_secrets {
                options = options.with_passphrase(self.sync_passphrase()?);
            }
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
        let mine = self.fingerprint(config, &applied.state)?;
        self.remember(remote_id, &seen, &mine)?;
        Ok(Step::Done(SyncOutcome::Merged(Box::new(pull_report(
            &plan,
            applied,
            from_remote && document.has_secrets() && !config.sync_secrets,
        )))))
    }

    fn use_remote(&self, config: &SyncConfig, client: &WebDavClient) -> Result<Step, SyncError> {
        let local = self.data.load().map_err(SyncError::Local)?;
        let remote = self
            .fetch(config, client)?
            .ok_or(SyncError::NothingInCloud)?;
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
        let mine = self.fingerprint(config, &local)?;
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
        let step = self.upload_to(
            config,
            client,
            &client.file_url(),
            &local,
            &mine,
            remote.as_ref(),
        )?;
        Ok(match step {
            Step::Done(SyncOutcome::Uploaded(mut report)) => {
                report.remote_snapshot = remote_snapshot;
                Step::Done(SyncOutcome::Uploaded(report))
            }
            other => other,
        })
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
        self.remember(remote_id, &seen, mine)?;
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
        let mine = self.fingerprint(config, &applied.state)?;
        self.remember(remote_id, &remote.seen(), &mine)?;
        Ok(Step::Done(SyncOutcome::Pulled(Box::new(pull_report(
            &plan,
            applied,
            document.has_secrets() && !config.sync_secrets,
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

    /// Records what both sides hold after a successful sync.
    fn remember(&self, remote_id: &str, seen: &Seen, mine: &Fingerprint) -> Result<(), SyncError> {
        let store = SyncStateStore::at(self.data.root());
        let mut state = store.load();
        state.remote = Some(remote_id.to_owned());
        state.last_remote_etag.clone_from(&seen.etag);
        state.last_remote_hash = Some(seen.mark.clone());
        state.last_remote_core_hash.clone_from(&seen.core_hash);
        state.last_synced_hash = Some(mine.view.clone());
        state.last_synced_core_hash = Some(mine.core.clone());
        state.last_secrets_hash.clone_from(&mine.secrets);
        store.save(&state)?;
        Ok(())
    }

    /// The fingerprint of the local data as this platform syncs it: the view of
    /// the document it would upload, and — when passwords are synced — a
    /// fingerprint of the saved passwords keyed by the sync passphrase (so the
    /// state file holds nothing a password could be guessed from without it).
    fn fingerprint(
        &self,
        config: &SyncConfig,
        local: &LocalState,
    ) -> Result<Fingerprint, SyncError> {
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
        let secrets = if config.sync_secrets {
            let passphrase = self.sync_passphrase()?;
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
            Some(secrets_digest(
                &passphrase,
                saved.iter().map(|(id, secret)| (*id, secret)),
            ))
        } else {
            None
        };
        Ok(Fingerprint {
            view,
            core,
            secrets,
        })
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
fn pull_report(plan: &ApplyPlan, applied: ApplyOutcome, secrets_ignored: bool) -> PullReport {
    PullReport {
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
/// when passwords are synced here (a new password changes only the block).
fn remote_mark(view_hash: &str, document: &[u8], with_secrets: bool) -> String {
    let block = with_secrets
        .then(|| serde_json::from_slice::<serde_json::Value>(document).ok())
        .flatten()
        .and_then(|value| value.get("secrets").map(ToString::to_string));
    match block {
        Some(block) => format!("{view_hash}+{}", hash(block.as_bytes())),
        None => view_hash.to_owned(),
    }
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
