//! Turning a portable document into local data: the preview, then the write.
//!
//! [`ApplyPlan::new`] computes everything — the combined records, the counts the
//! user confirms, the private-key paths that came from another machine — without
//! touching a file. [`ApplyPlan::apply`] then writes exactly what was previewed:
//! it copies every data file into a snapshot first, writes the changed files one
//! by one, saves the opened passwords, and puts every earlier write back when a
//! later one fails.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::path::{Path, PathBuf};

use serde::Serialize;
use shellune_core::{
    ConnectionRecord, ForwardBook, GroupBook, HostConfig, HostId, SNIPPETS_FORMAT_VERSION,
    SecretString, SnippetBook,
};

use super::{
    HostOverlay, LocalData, LocalState, Platform, PlatformData, PortableError, SecretBundle,
    ShelluneData, now_unix,
};
use crate::book::{FORMAT_VERSION, HostBook, RecentConnection};
use crate::credentials::SecretStore;
use crate::preferences::Preferences;
use crate::store::write_atomic_file;
use crate::trust::{HostTrust, TRUST_FORMAT_VERSION, TrustEntry};

/// Every data file a document covers, in the order they are written: the host
/// records first, because they are what a user cannot recreate.
const DATA_FILES: [&str; 6] = [
    crate::store::FILE_NAME,
    crate::group::FILE_NAME,
    crate::forwarding::FILE_NAME,
    crate::snippets::FILE_NAME,
    crate::trust::TRUST_FILE_NAME,
    crate::preferences::FILE_NAME,
];

/// Writes one data file (directory, name, bytes); replaced in tests to make a
/// chosen write fail.
type FileWriter<'a> = dyn FnMut(&Path, &str, &[u8]) -> Result<(), String> + 'a;

/// How a document meets the local data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyMode {
    /// Records are matched by identifier: the document's version of a record
    /// wins, records only this machine has are kept, recents keep the newer time
    /// and accepted server identities are combined.
    Merge,
    /// The local records are replaced by the document's as a whole.
    Replace,
}

/// How many records of one kind a plan adds, changes and removes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// Records the document brings that this machine does not have.
    pub added: usize,
    /// Records both have, which the document changes.
    pub updated: usize,
    /// Records this machine has that the plan removes.
    pub removed: usize,
}

impl Change {
    /// Whether nothing of this kind changes.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.added == 0 && self.updated == 0 && self.removed == 0
    }
}

/// A machine-specific field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformField {
    /// The private key file of an SSH host.
    IdentityFile,
    /// The device of a serial connection.
    SerialDevice,
}

/// A record whose machine-specific field came from another machine.
///
/// The document had no section for this platform naming the record, and this
/// machine did not have the record either, so the value the other machine used is
/// kept. The interface checks whether that file exists here and says so; the
/// record is neither dropped nor blocked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformFallback {
    /// The record.
    pub id: HostId,
    /// Its name, for the preview.
    pub name: String,
    /// Which field.
    pub field: PlatformField,
    /// The value taken from the shared record.
    pub value: String,
}

/// What applying a document would do, computed without writing anything.
pub struct ApplyPlan {
    mode: ApplyMode,
    platform: Platform,
    source_platform: String,
    source_app_version: String,
    created_unix: u64,
    has_secrets: bool,
    hosts: Change,
    groups: Change,
    forwards: Change,
    snippets: Change,
    known_hosts: Change,
    preferences_changed: bool,
    keymap_changed: bool,
    fallbacks: Vec<PlatformFallback>,
    secrets: Vec<(HostId, SecretString)>,
    next: LocalState,
    files: Vec<(&'static str, Vec<u8>)>,
}

impl fmt::Debug for ApplyPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplyPlan")
            .field("mode", &self.mode)
            .field("platform", &self.platform)
            .field("source_platform", &self.source_platform)
            .field("hosts", &self.hosts)
            .field("groups", &self.groups)
            .field("forwards", &self.forwards)
            .field("snippets", &self.snippets)
            .field("known_hosts", &self.known_hosts)
            .field("preferences_changed", &self.preferences_changed)
            .field("secrets", &self.secrets.len())
            .field("files", &self.files().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// What an applied plan did.
#[derive(Clone, Debug)]
pub struct ApplyOutcome {
    /// The snapshot holding the files as they were; `None` when nothing needed
    /// writing.
    pub snapshot: Option<PathBuf>,
    /// The data files that were written.
    pub files_written: Vec<&'static str>,
    /// How many passwords were saved to the credential store.
    pub secrets_saved: usize,
    /// The local data as it now is, for the interface to reload.
    pub state: LocalState,
}

impl ApplyPlan {
    /// Plans applying `document` to `local` on `platform`.
    ///
    /// `secrets` are the passwords opened from the document; each one whose
    /// record ends up in the combined book and can remember a password is planned
    /// to be saved, and its record is marked as remembering it.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Corrupt`] when the combined records break a rule
    /// of the stores, which a document that passed [`ShelluneData::from_json`] and
    /// a local state that loaded cleanly do not.
    pub fn new(
        document: &ShelluneData,
        local: &LocalState,
        platform: Platform,
        mode: ApplyMode,
        secrets: Option<&SecretBundle>,
    ) -> Result<Self, PortableError> {
        let shared = document.shared();
        let section = document.platform(platform);
        let mut fallbacks = Vec::new();
        let (hosts, secrets) = plan_book(
            shared.hosts(),
            &local.hosts,
            section.as_ref(),
            mode,
            secrets,
            &mut fallbacks,
        )?;
        let groups = plan_groups(shared.groups(), &local.groups, mode)?;
        let forwards = plan_forwards(shared.forwards(), &local.forwards, mode)?;
        let snippets = plan_snippets(shared.snippets(), &local.snippets, mode)?;
        let known_hosts = plan_trust(shared.known_hosts(), &local.known_hosts, mode)?;
        let preferences = plan_preferences(
            document,
            &local.preferences,
            section.as_ref(),
            platform,
            mode,
        );
        let next = LocalState {
            hosts,
            groups,
            forwards,
            snippets,
            known_hosts,
            preferences,
        };

        let keymap_of = |preferences: &Preferences| {
            preferences
                .keymap
                .as_ref()
                .map(|keymap| keymap.for_platform(platform.as_str()).to_vec())
                .unwrap_or_default()
        };
        let plan = Self {
            mode,
            platform,
            source_platform: document.source_platform().to_owned(),
            source_app_version: document.source_app_version().to_owned(),
            created_unix: document.created_unix(),
            has_secrets: document.has_secrets(),
            hosts: diff(
                local.hosts.records().into_iter().map(|r| (r.id(), r)),
                next.hosts.records().into_iter().map(|r| (r.id(), r)),
            ),
            groups: diff(
                local.groups.paths().iter().map(|path| (path.clone(), ())),
                next.groups.paths().iter().map(|path| (path.clone(), ())),
            ),
            forwards: diff(
                local.forwards.rules().iter().map(|rule| (rule.id(), rule)),
                next.forwards.rules().iter().map(|rule| (rule.id(), rule)),
            ),
            snippets: diff(
                local.snippets.snippets().iter().map(|s| (s.id(), s)),
                next.snippets.snippets().iter().map(|s| (s.id(), s)),
            ),
            known_hosts: diff(
                local.known_hosts.entries().iter().map(trust_key),
                next.known_hosts.entries().iter().map(trust_key),
            ),
            preferences_changed: next.preferences != local.preferences,
            keymap_changed: keymap_of(&next.preferences) != keymap_of(&local.preferences),
            fallbacks,
            secrets,
            files: changed_files(local, &next)?,
            next,
        };
        Ok(plan)
    }

    /// Merge or replace.
    #[must_use]
    pub fn mode(&self) -> ApplyMode {
        self.mode
    }

    /// The platform the plan applies to.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The platform the document was made on, as written in it.
    #[must_use]
    pub fn source_platform(&self) -> &str {
        &self.source_platform
    }

    /// The application version that made the document.
    #[must_use]
    pub fn source_app_version(&self) -> &str {
        &self.source_app_version
    }

    /// When the document was made.
    #[must_use]
    pub fn created_unix(&self) -> u64 {
        self.created_unix
    }

    /// Whether the document carries sealed passwords.
    #[must_use]
    pub fn has_secrets(&self) -> bool {
        self.has_secrets
    }

    /// How many passwords applying will save.
    #[must_use]
    pub fn secrets_to_import(&self) -> usize {
        self.secrets.len()
    }

    /// Saved connections of every protocol.
    #[must_use]
    pub fn hosts(&self) -> Change {
        self.hosts
    }

    /// Groups (only added or removed; a group has nothing to update).
    #[must_use]
    pub fn groups(&self) -> Change {
        self.groups
    }

    /// Forwarding rules.
    #[must_use]
    pub fn forwards(&self) -> Change {
        self.forwards
    }

    /// Saved commands.
    #[must_use]
    pub fn snippets(&self) -> Change {
        self.snippets
    }

    /// Accepted server identities.
    #[must_use]
    pub fn known_hosts(&self) -> Change {
        self.known_hosts
    }

    /// Whether any preference changes, key bindings included.
    #[must_use]
    pub fn preferences_changed(&self) -> bool {
        self.preferences_changed
    }

    /// Whether this platform's key bindings change.
    #[must_use]
    pub fn keymap_changed(&self) -> bool {
        self.keymap_changed
    }

    /// Records whose private-key path or serial device came from another machine.
    #[must_use]
    pub fn fallbacks(&self) -> &[PlatformFallback] {
        &self.fallbacks
    }

    /// The local data as it will be once applied.
    #[must_use]
    pub fn result(&self) -> &LocalState {
        &self.next
    }

    /// The data files applying will write.
    pub fn files(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.files.iter().map(|(name, _)| *name)
    }

    /// Whether applying would change nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.secrets.is_empty()
    }

    /// Writes the plan into the data directory.
    ///
    /// Every data file is copied into a new snapshot (named after `reason`)
    /// before the first write; the changed files are then written one by one,
    /// each atomically, and the planned passwords are saved. When a step fails,
    /// every file already written gets its previous bytes back (or is removed if
    /// it did not exist) and every password already saved gets its previous value
    /// back, so the data is what it was before.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Snapshot`] when the snapshot could not be made
    /// (nothing was written), and [`PortableError::Apply`] when a later step failed.
    pub fn apply(
        &self,
        data: &LocalData,
        secrets: &dyn SecretStore,
        reason: &str,
    ) -> Result<ApplyOutcome, PortableError> {
        self.apply_with(data, secrets, reason, &mut |root, name, bytes| {
            write_atomic_file(root, name, bytes).map_err(|error| error.to_string())
        })
    }

    fn apply_with(
        &self,
        data: &LocalData,
        secret_store: &dyn SecretStore,
        reason: &str,
        write: &mut FileWriter<'_>,
    ) -> Result<ApplyOutcome, PortableError> {
        if self.is_empty() {
            return Ok(ApplyOutcome {
                snapshot: None,
                files_written: Vec::new(),
                secrets_saved: 0,
                state: self.next.clone(),
            });
        }
        let root = data.root();
        let mut previous: Vec<(&'static str, Option<Vec<u8>>)> = Vec::new();
        for name in DATA_FILES {
            let path = root.join(name);
            match std::fs::read(&path) {
                Ok(bytes) => previous.push((name, Some(bytes))),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    previous.push((name, None));
                }
                Err(error) => {
                    return Err(PortableError::Snapshot {
                        path,
                        reason: error.to_string(),
                    });
                }
            }
        }
        let copies: Vec<(&str, &[u8])> = previous
            .iter()
            .filter_map(|(name, bytes)| bytes.as_deref().map(|bytes| (*name, bytes)))
            .collect();
        let snapshot = data.snapshots().create(reason, now_unix(), &copies)?;

        let mut written: Vec<&'static str> = Vec::new();
        for (name, bytes) in &self.files {
            if let Err(reason) = write(root, name, bytes) {
                let restored = restore_files(root, &written, &previous);
                return Err(PortableError::Apply {
                    step: format!("Writing {name}"),
                    reason,
                    restored,
                    snapshot,
                });
            }
            written.push(name);
        }

        let mut saved: Vec<(HostId, Option<SecretString>)> = Vec::new();
        for (id, secret) in &self.secrets {
            let outcome = secret_store
                .load(*id)
                .and_then(|before| secret_store.save(*id, secret).map(|()| before));
            match outcome {
                Ok(before) => saved.push((*id, before)),
                Err(error) => {
                    let secrets_restored = restore_secrets(secret_store, &saved);
                    let files_restored = restore_files(root, &written, &previous);
                    return Err(PortableError::Apply {
                        step: "Saving the passwords".to_owned(),
                        reason: error.to_string(),
                        restored: secrets_restored && files_restored,
                        snapshot,
                    });
                }
            }
        }

        Ok(ApplyOutcome {
            snapshot: Some(snapshot),
            files_written: written,
            secrets_saved: saved.len(),
            state: self.next.clone(),
        })
    }
}

/// Puts back the files written so far; true when every one was restored.
fn restore_files(
    root: &Path,
    written: &[&'static str],
    previous: &[(&'static str, Option<Vec<u8>>)],
) -> bool {
    let mut restored = true;
    for name in written.iter().rev() {
        let before = previous
            .iter()
            .find(|(file, _)| file == name)
            .and_then(|(_, bytes)| bytes.as_deref());
        let outcome = match before {
            Some(bytes) => write_atomic_file(root, name, bytes).is_ok(),
            None => match std::fs::remove_file(root.join(name)) {
                Ok(()) => true,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            },
        };
        restored &= outcome;
    }
    restored
}

/// Puts back the passwords saved so far; true when every one was restored.
fn restore_secrets(store: &dyn SecretStore, saved: &[(HostId, Option<SecretString>)]) -> bool {
    let mut restored = true;
    for (id, before) in saved.iter().rev() {
        let outcome = match before {
            Some(secret) => store.save(*id, secret),
            None => store.forget(*id),
        };
        restored &= outcome.is_ok();
    }
    restored
}

/// The combined host book and the passwords to save with it.
fn plan_book(
    incoming: &HostBook,
    local: &HostBook,
    section: Option<&PlatformData>,
    mode: ApplyMode,
    secrets: Option<&SecretBundle>,
    fallbacks: &mut Vec<PlatformFallback>,
) -> Result<(HostBook, Vec<(HostId, SecretString)>), PortableError> {
    let local_records: HashMap<HostId, ConnectionRecord> = local
        .records()
        .into_iter()
        .map(|record| (record.id(), record))
        .collect();
    let mut arriving = Vec::with_capacity(incoming.connection_count());
    for record in incoming.records() {
        let overlay = section.and_then(|section| section.hosts.get(&record.id()));
        let mine = local_records.get(&record.id());
        arriving.push(resolve_platform(record, overlay, mine, fallbacks)?);
    }

    let (mut records, recents, favorites) = match mode {
        ApplyMode::Replace => (
            arriving,
            incoming.recents().to_vec(),
            incoming.favorites().to_vec(),
        ),
        ApplyMode::Merge => {
            let mut records = local.records();
            let positions: HashMap<HostId, usize> = records
                .iter()
                .enumerate()
                .map(|(index, record)| (record.id(), index))
                .collect();
            let arriving_ids: HashSet<HostId> = arriving.iter().map(ConnectionRecord::id).collect();
            for record in arriving {
                match positions.get(&record.id()) {
                    Some(&index) => records[index] = record,
                    None => records.push(record),
                }
            }
            // A record the document carries is a favourite exactly when the
            // document says so; a record only this machine has keeps its mark.
            let mut favorites: Vec<HostId> = local
                .favorites()
                .iter()
                .copied()
                .filter(|id| !arriving_ids.contains(id) || incoming.is_favorite(*id))
                .collect();
            for id in incoming.favorites() {
                if !favorites.contains(id) {
                    favorites.push(*id);
                }
            }
            (
                records,
                merge_recents(local.recents(), incoming.recents()),
                favorites,
            )
        }
    };

    let mut to_save = Vec::new();
    if let Some(bundle) = secrets {
        for record in &mut records {
            let Some(secret) = bundle.get(record.id()) else {
                continue;
            };
            match record {
                ConnectionRecord::Ssh(host) => host.set_remember_secret(true),
                ConnectionRecord::Rdp(config) => config.set_remember_secret(true),
                ConnectionRecord::Vnc(config) => config.set_remember_secret(true),
                ConnectionRecord::Serial(_) => {}
            }
            // An SSH host that signs in without a password cannot remember one, so
            // its password is not saved either.
            if record.remembers_secret() {
                to_save.push((record.id(), secret.clone()));
            }
        }
    }

    Ok((build_book(&records, &recents, &favorites)?, to_save))
}

/// Puts this machine's private-key path or serial device into a record.
///
/// The platform's own section wins; a record this machine already has keeps its
/// own value; otherwise the shared value stays and is reported.
fn resolve_platform(
    record: ConnectionRecord,
    overlay: Option<&HostOverlay>,
    mine: Option<&ConnectionRecord>,
    fallbacks: &mut Vec<PlatformFallback>,
) -> Result<ConnectionRecord, PortableError> {
    match record {
        ConnectionRecord::Ssh(mut host) => {
            if let Some(overlay) = overlay {
                host.set_identity_file(overlay.identity_file.clone());
            } else if let Some(ConnectionRecord::Ssh(mine)) = mine {
                host.set_identity_file(mine.identity_file().map(str::to_owned));
            } else if let Some(path) = host.identity_file() {
                fallbacks.push(PlatformFallback {
                    id: host.id(),
                    name: host.name().to_owned(),
                    field: PlatformField::IdentityFile,
                    value: path.to_owned(),
                });
            }
            Ok(ConnectionRecord::Ssh(host))
        }
        ConnectionRecord::Serial(mut serial) => {
            let device = overlay
                .and_then(|overlay| overlay.serial_device.clone())
                .or_else(|| match mine {
                    Some(ConnectionRecord::Serial(mine)) => Some(mine.device().to_owned()),
                    _ => None,
                });
            match device {
                Some(device) => {
                    serial
                        .set_device(device)
                        .map_err(|error| PortableError::Corrupt {
                            reason: format!(
                                "the device of the serial connection {} is unusable: {error}",
                                serial.name()
                            ),
                        })?;
                }
                None => fallbacks.push(PlatformFallback {
                    id: serial.id(),
                    name: serial.name().to_owned(),
                    field: PlatformField::SerialDevice,
                    value: serial.device().to_owned(),
                }),
            }
            Ok(ConnectionRecord::Serial(serial))
        }
        other => Ok(other),
    }
}

/// One record per host, keeping the newer connection time.
fn merge_recents(
    local: &[RecentConnection],
    incoming: &[RecentConnection],
) -> Vec<RecentConnection> {
    let mut merged: Vec<RecentConnection> = local.to_vec();
    for recent in incoming {
        match merged
            .iter_mut()
            .find(|saved| saved.host() == recent.host())
        {
            Some(saved) if saved.last_connected_unix() < recent.last_connected_unix() => {
                *saved = *recent;
            }
            Some(_) => {}
            None => merged.push(*recent),
        }
    }
    merged.sort_by_key(|recent| std::cmp::Reverse(recent.last_connected_unix()));
    merged
}

/// Builds a host book through its stored form, so it passes the same checks a
/// loaded file does.
fn build_book(
    records: &[ConnectionRecord],
    recents: &[RecentConnection],
    favorites: &[HostId],
) -> Result<HostBook, PortableError> {
    #[derive(Serialize)]
    struct BookWire<'a> {
        version: u32,
        hosts: Vec<&'a HostConfig>,
        connections: Vec<&'a ConnectionRecord>,
        recents: Vec<&'a RecentConnection>,
        favorites: Vec<HostId>,
    }

    let ids: HashSet<HostId> = records.iter().map(ConnectionRecord::id).collect();
    let mut marked = HashSet::new();
    let wire = BookWire {
        version: FORMAT_VERSION,
        hosts: records
            .iter()
            .filter_map(ConnectionRecord::as_ssh)
            .collect(),
        connections: records.iter().filter(|record| !record.is_ssh()).collect(),
        recents: recents
            .iter()
            .filter(|recent| ids.contains(&recent.host()))
            .collect(),
        favorites: favorites
            .iter()
            .copied()
            .filter(|id| ids.contains(id) && marked.insert(*id))
            .collect(),
    };
    let book: HostBook = serde_json::to_value(&wire)
        .and_then(serde_json::from_value)
        .map_err(|error| PortableError::Corrupt {
            reason: format!("the combined hosts cannot be stored: {error}"),
        })?;
    book.ensure_valid()
        .map_err(|reason| PortableError::Corrupt { reason })?;
    Ok(book)
}

fn plan_groups(
    incoming: &GroupBook,
    local: &GroupBook,
    mode: ApplyMode,
) -> Result<GroupBook, PortableError> {
    if mode == ApplyMode::Replace {
        return Ok(incoming.clone());
    }
    let mut groups = local.clone();
    for path in incoming.paths() {
        if !groups.contains(path) {
            groups
                .insert(path)
                .map_err(|error| PortableError::Corrupt {
                    reason: error.to_string(),
                })?;
        }
    }
    Ok(groups)
}

fn plan_forwards(
    incoming: &ForwardBook,
    local: &ForwardBook,
    mode: ApplyMode,
) -> Result<ForwardBook, PortableError> {
    if mode == ApplyMode::Replace {
        return Ok(incoming.clone());
    }
    let mut forwards = local.clone();
    for rule in incoming.rules() {
        let outcome = if forwards.rule(rule.id()).is_some() {
            forwards.update(rule.clone())
        } else {
            forwards.insert(rule.clone())
        };
        outcome.map_err(|error| PortableError::Corrupt {
            reason: error.to_string(),
        })?;
    }
    Ok(forwards)
}

fn plan_snippets(
    incoming: &SnippetBook,
    local: &SnippetBook,
    mode: ApplyMode,
) -> Result<SnippetBook, PortableError> {
    if mode == ApplyMode::Replace {
        return Ok(incoming.clone());
    }
    let merged = merge_by(local.snippets(), incoming.snippets(), |snippet| {
        snippet.id()
    });
    let book: SnippetBook = serde_json::from_value(serde_json::json!({
        "version": SNIPPETS_FORMAT_VERSION,
        "snippets": merged,
    }))
    .map_err(|error| PortableError::Corrupt {
        reason: format!("the combined saved commands cannot be stored: {error}"),
    })?;
    book.validate()
        .map_err(|reason| PortableError::Corrupt { reason })?;
    Ok(book)
}

fn plan_trust(
    incoming: &HostTrust,
    local: &HostTrust,
    mode: ApplyMode,
) -> Result<HostTrust, PortableError> {
    if mode == ApplyMode::Replace {
        return Ok(incoming.clone());
    }
    let merged = merge_by(local.entries(), incoming.entries(), |entry| {
        (entry.host().to_owned(), entry.port())
    });
    let trust: HostTrust = serde_json::from_value(serde_json::json!({
        "version": TRUST_FORMAT_VERSION,
        "entries": merged,
    }))
    .map_err(|error| PortableError::Corrupt {
        reason: format!("the combined server identities cannot be stored: {error}"),
    })?;
    trust
        .validate()
        .map_err(|reason| PortableError::Corrupt { reason })?;
    Ok(trust)
}

fn plan_preferences(
    document: &ShelluneData,
    local: &Preferences,
    section: Option<&PlatformData>,
    platform: Platform,
    mode: ApplyMode,
) -> Preferences {
    let merge = mode == ApplyMode::Merge;
    let mut preferences = local.clone();
    document
        .shared()
        .preferences()
        .apply_to(&mut preferences, merge);
    // Without a section for this platform the document knows nothing about this
    // machine's screen or keyboard, so those choices stay as they are.
    if let Some(section) = section {
        if let Some(entries) = &section.keymap {
            let mut keymap = preferences.keymap.take().unwrap_or_default();
            keymap.set_for_platform(platform.as_str(), entries.clone());
            preferences.keymap = (!keymap.is_empty()).then_some(keymap);
        }
        if !merge || section.text_size.is_some() {
            preferences.text_size = section.text_size;
        }
        if !merge || section.sidebar_collapsed.is_some() {
            preferences.sidebar_collapsed = section.sidebar_collapsed;
        }
    }
    preferences
}

/// Records of one kind, matched by `key`: the local order, each record replaced
/// by the incoming one with the same key, then the incoming records that are new.
fn merge_by<T: Clone, K: Eq + Hash>(local: &[T], incoming: &[T], key: impl Fn(&T) -> K) -> Vec<T> {
    let mut merged: Vec<T> = local.to_vec();
    let positions: HashMap<K, usize> = merged
        .iter()
        .enumerate()
        .map(|(index, item)| (key(item), index))
        .collect();
    for item in incoming {
        match positions.get(&key(item)) {
            Some(&index) => merged[index] = item.clone(),
            None => merged.push(item.clone()),
        }
    }
    merged
}

/// Counts what changes between two keyed collections.
fn diff<K: Eq + Hash, T: PartialEq>(
    before: impl Iterator<Item = (K, T)>,
    after: impl Iterator<Item = (K, T)>,
) -> Change {
    let before: HashMap<K, T> = before.collect();
    let mut change = Change::default();
    let mut seen = HashSet::new();
    for (key, item) in after {
        match before.get(&key) {
            None => change.added += 1,
            Some(previous) if *previous != item => change.updated += 1,
            Some(_) => {}
        }
        seen.insert(key);
    }
    change.removed = before.keys().filter(|key| !seen.contains(*key)).count();
    change
}

fn trust_key(entry: &TrustEntry) -> ((String, u16), &TrustEntry) {
    ((entry.host().to_owned(), entry.port()), entry)
}

/// The data files whose content differs, with the bytes to write.
fn changed_files(
    local: &LocalState,
    next: &LocalState,
) -> Result<Vec<(&'static str, Vec<u8>)>, PortableError> {
    fn bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, PortableError> {
        serde_json::to_vec_pretty(value).map_err(|error| PortableError::Encode {
            reason: error.to_string(),
        })
    }
    let pairs = [
        (DATA_FILES[0], bytes(&local.hosts)?, bytes(&next.hosts)?),
        (DATA_FILES[1], bytes(&local.groups)?, bytes(&next.groups)?),
        (
            DATA_FILES[2],
            bytes(&local.forwards)?,
            bytes(&next.forwards)?,
        ),
        (
            DATA_FILES[3],
            bytes(&local.snippets)?,
            bytes(&next.snippets)?,
        ),
        (
            DATA_FILES[4],
            bytes(&local.known_hosts)?,
            bytes(&next.known_hosts)?,
        ),
        (
            DATA_FILES[5],
            bytes(&local.preferences)?,
            bytes(&next.preferences)?,
        ),
    ];
    Ok(pairs
        .into_iter()
        .filter(|(_, before, after)| before != after)
        .map(|(name, _, after)| (name, after))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemorySecrets;
    use crate::portable::ExportOptions;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shellune-apply-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn state_with(names: &[&str]) -> LocalState {
        let mut state = LocalState::default();
        for name in names {
            state
                .hosts
                .insert(HostConfig::new(*name, "10.0.0.1", 22, "me").expect("host"))
                .expect("inserted");
        }
        state.snippets.create("one", "echo one").expect("snippet");
        state.preferences.theme = Some("dark".to_owned());
        state
    }

    #[test]
    fn a_failed_write_puts_back_every_file_already_written() {
        let root = scratch("rollback");
        let data = LocalData::at(&root);
        // The machine has hosts and preferences, but no snippet file yet.
        let mut local = state_with(&["old"]);
        local.snippets = SnippetBook::new();
        crate::HostStore::at(&root)
            .save(&local.hosts)
            .expect("hosts");
        crate::PreferencesStore::at(&root)
            .save(&local.preferences)
            .expect("preferences");
        let before_hosts = std::fs::read(root.join("hosts.json")).expect("hosts");
        let before_preferences = std::fs::read(root.join("preferences.json")).expect("prefs");
        let local = data.load().expect("loads");

        let mut incoming = state_with(&["new-a", "new-b"]);
        incoming.preferences.theme = Some("light".to_owned());
        let document = ShelluneData::export(
            &incoming,
            &ExportOptions::new(Platform::Windows),
            &MemorySecrets::new(),
            None,
        )
        .expect("exported")
        .document;
        let plan = document
            .plan(&local, Platform::Windows, ApplyMode::Replace, None)
            .expect("planned");
        assert_eq!(
            plan.files().collect::<Vec<_>>(),
            vec!["hosts.json", "snippets.json", "preferences.json"]
        );

        // Hosts and snippets are written for real; the preferences write fails.
        let mut calls = Vec::new();
        let error = plan
            .apply_with(
                &data,
                &MemorySecrets::new(),
                "import",
                &mut |root, name, bytes| {
                    calls.push(name.to_owned());
                    if name == "preferences.json" {
                        return Err("the disk is full".to_owned());
                    }
                    write_atomic_file(root, name, bytes).map_err(|error| error.to_string())
                },
            )
            .expect_err("the third write fails");
        assert_eq!(
            calls,
            vec!["hosts.json", "snippets.json", "preferences.json"]
        );
        let PortableError::Apply {
            restored,
            snapshot,
            step,
            reason,
        } = &error
        else {
            panic!("{error}");
        };
        assert!(restored, "{error}");
        assert!(step.contains("preferences.json"));
        assert_eq!(reason, "the disk is full");
        assert!(error.to_string().contains("put back"), "{error}");
        assert_eq!(
            std::fs::read(root.join("hosts.json")).expect("hosts"),
            before_hosts,
            "the written host file has its previous bytes"
        );
        assert!(
            !root.join("snippets.json").exists(),
            "a file that did not exist before is removed again"
        );
        assert_eq!(
            std::fs::read(root.join("preferences.json")).expect("prefs"),
            before_preferences
        );
        assert_eq!(
            std::fs::read(snapshot.join("hosts.json")).expect("copied"),
            before_hosts
        );
        assert_eq!(data.load().expect("loads"), local);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_snapshot_that_cannot_be_made_stops_before_any_write() {
        let root = scratch("no-snapshot");
        let data = LocalData::at(&root);
        std::fs::create_dir_all(&root).expect("dir");
        // A file where the snapshot directory belongs.
        std::fs::write(root.join(crate::portable::SNAPSHOT_DIRECTORY), b"x").expect("blocker");
        let document = ShelluneData::export(
            &state_with(&["a"]),
            &ExportOptions::new(Platform::Linux),
            &MemorySecrets::new(),
            None,
        )
        .expect("exported")
        .document;
        let plan = document
            .plan(
                &LocalState::default(),
                Platform::Linux,
                ApplyMode::Merge,
                None,
            )
            .expect("planned");
        let mut writes = 0;
        let error = plan
            .apply_with(&data, &MemorySecrets::new(), "import", &mut |_, _, _| {
                writes += 1;
                Ok(())
            })
            .expect_err("no snapshot, no write");
        assert!(matches!(error, PortableError::Snapshot { .. }), "{error}");
        assert_eq!(writes, 0);
        assert!(!root.join("hosts.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn counting_changes_by_key() {
        let change = diff(
            [(1, "a"), (2, "b"), (3, "c")].into_iter(),
            [(1, "a"), (2, "B"), (4, "d")].into_iter(),
        );
        assert_eq!(
            change,
            Change {
                added: 1,
                updated: 1,
                removed: 1
            }
        );
        assert!(!change.is_empty());
        assert!(Change::default().is_empty());
    }
}
