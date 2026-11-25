//! WebDAV sync in the window: the settings' sync category, automatic sync, the conflict
//! question and the marker in the top bar (R7, R8, R9, R10).
//!
//! The sync engine (`shellune-sync`) decides what one sync does - nothing, an upload, a
//! pull or a conflict - and every one of its calls blocks on the network and the disk.
//! This module decides *when* a sync runs and keeps the window responsive while it does:
//!
//! * one job at a time, on the background executor. The engine holds no lock, so an
//!   automatic sync and a press of "Upload now" must never overlap: a job asked for while
//!   another runs waits in a single slot (a manual job replaces a waiting automatic one,
//!   never the other way round) and starts when the running one ends;
//! * automatic sync is only ever started when a server is saved and the automatic switch
//!   is on: a pull once the stores are loaded at start, a pull every
//!   [`POLL_INTERVAL`], and an upload [`DEBOUNCE`] after the last saved change;
//! * the save points of the data that travels (hosts, groups, favourites, recents,
//!   forwards, saved commands, trusted keys, preferences) call
//!   [`ShelluneView::note_data_changed`]; the workspace snapshot does not, and nothing
//!   applied from the cloud is noted while it is being adopted, so a pull never echoes
//!   back as an upload;
//! * a job that may write the data files (a check, a pull, "use cloud data") and the
//!   application of a backup share that one slot, so two writers never apply at once.
//!   While such a writer runs - from its start until the window has read back what it
//!   wrote - the window's own saves are *held back*: the edit shows at once, but its
//!   file is not written over the one the writer may have just replaced. When the
//!   writer is done the held edits are merged onto the files as they are then
//!   (`LocalData::rebase`), written, read back, and uploaded like any other change; a
//!   stale copy in memory never overwrites records that just arrived (R8, R10);
//! * closing the window or quitting while such saves are held back waits for the
//!   writer (bounded by the engine's timeouts), writes the held saves, and only then
//!   closes, so an edit the window already showed as saved is never lost on exit;
//! * a failure never blocks the window or loses a local change: the change is already on
//!   disk, the reason is shown in the settings and as a marker in the top bar, and sync
//!   tries again after [`RETRY_FIRST`], doubling the wait up to [`RETRY_MAX`], as well
//!   as after the next change or check;
//! * a conflict is a question with three answers, and nothing is overwritten before the
//!   user answers it. Dismissing it keeps the conflict (and the marker) until it is
//!   answered; automatic checks do not ask again until it is, a manual one does.

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Div, Entity, FontWeight, SharedString, Stateful, Window, div, px};
use shellune_core::{ForwardBook, SecretString};
use shellune_storage::{
    ForwardStore, GroupService, HostBook, KdfParams, LocalData, LocalState, PersistError,
    Platform as DataPlatform, PortableError, Preferences, PreferencesStore, SecretStore,
    StorageError, Touched,
};
use shellune_sync::{
    ConflictReport, LastResult, PullReport, Resolution, SyncConfig, SyncError, SyncOutcome,
    SyncService, SyncState, WebDavError,
};

use super::ShelluneView;
use super::settings::Category;
use super::shell::{ModalContent, NoticeForm, notice_icon, notice_words};
use crate::app::components::{self, Variant};
use crate::app::copy::{self, SyncStrings};
use crate::app::icons::Icon;
use crate::app::nav::Destination;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Tone, palette, radius, spacing};

/// How long after the last saved change an automatic upload starts. Edits made in a
/// burst become one upload.
pub(super) const DEBOUNCE: Duration = Duration::from_secs(5);

/// How often the cloud is checked while automatic sync is on.
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// The wait before the first automatic retry after a failed sync.
pub(super) const RETRY_FIRST: Duration = Duration::from_secs(60);

/// The longest wait between automatic retries; each failure doubles the wait up to it.
pub(super) const RETRY_MAX: Duration = Duration::from_secs(15 * 60);

/// Work that waits for the slot the sync jobs use (a backup import).
type SlotTask = Box<dyn FnOnce(&mut ShelluneView, &mut Context<ShelluneView>)>;

/// The window's saves, held back while another writer replaces the data files.
struct DataGate {
    /// The data as the window held it when the writer started. The held edits are
    /// exactly what the window changed since, so they are what is merged onto the
    /// files the writer leaves; taken from memory, so starting a writer reads no file.
    base: Box<LocalState>,
    /// The documents the window changed meanwhile.
    touched: Touched,
}

/// What became of the saves held back during a writer.
#[derive(Debug, Default)]
pub(super) struct HeldWrites {
    /// Some file was written.
    written: bool,
    /// What is on disk now differs from what the window shows, so it is read back.
    reload: bool,
    /// The preferences are among what is read back.
    preferences: bool,
    /// Documents whose edits could not be combined with what arrived.
    dropped: Vec<&'static str>,
    /// Why the held edits could not be written at all.
    failed: Option<String>,
}

/// What waits for the held saves to be written before it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingExit {
    /// The window closes.
    Window,
    /// The application quits (macOS asks through `applicationShouldTerminate:`).
    Quit,
}

/// A group change made while writes are held back goes through a scratch copy of the
/// data directory, so the service's own writes do not touch the real files.
pub(super) struct GroupWriter {
    pub(super) service: GroupService,
    scratch: Option<PathBuf>,
}

/// One sync the window asks the engine for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SyncJob {
    /// Whatever the comparison says, started by the window (start, change, check).
    Auto,
    /// Whatever the comparison says, asked for by the user.
    Check,
    /// "Upload now".
    Upload,
    /// "Pull from cloud".
    Pull,
    /// The answer to a conflict.
    Resolve(Resolution),
    /// The answers to a conflict one place at a time: the vault records
    /// (ADR-5) and the records and settings of the data, by their keys. The
    /// engine writes nothing until every one is decided.
    ResolveRecords(
        Vec<shellune_sync::VaultChoice>,
        Vec<(String, shellune_storage::MergeSide)>,
    ),
    /// The merge that repairs a cloud vault older than this computer's.
    RepairRollback,
}

impl SyncJob {
    /// Whether a person asked for it, so its outcome is worth a notification.
    fn manual(&self) -> bool {
        !matches!(self, Self::Auto)
    }

    /// Whether it may write the data files of this computer: anything that can end in
    /// a pull. An upload and "use this computer's data" only write the cloud.
    fn writes_local(&self) -> bool {
        matches!(
            self,
            Self::Auto | Self::Check | Self::Pull | Self::Resolve(Resolution::UseRemote)
        ) || matches!(self, Self::ResolveRecords(..) | Self::RepairRollback)
    }
}

/// Where the connection test is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TestStatus {
    Idle,
    Running,
    Passed,
    Failed(String),
}

/// The sync state of one window.
pub(super) struct SyncCenter {
    service: SyncService,
    /// The saved settings; `None` when sync was never set up.
    config: Option<SyncConfig>,
    /// What the engine remembers about the last attempt.
    state: SyncState,
    url: Entity<TextField>,
    username: Entity<TextField>,
    password: Entity<TextField>,
    remote_dir: Entity<TextField>,
    passphrase: Entity<TextField>,
    /// The automatic switch as shown (saved at once when a server is saved).
    draft_auto: bool,
    password_saved: bool,
    passphrase_saved: bool,
    /// The job on the background executor.
    running: Option<SyncJob>,
    /// The job that starts when the running one ends.
    queued: Option<SyncJob>,
    /// Bumped by every change; a debounce timer only fires for the newest one.
    change_generation: u64,
    /// Data from the cloud is being adopted: its reloads are not local changes.
    applying: bool,
    /// The polling loop is running.
    polling: bool,
    /// Whether this window may reach a server at all. Captures draw the page without.
    network: bool,
    /// Why the last attempt of this run failed, in the window's language.
    failure: Option<String>,
    test: TestStatus,
    /// Why the form refused, or a note about the last save.
    form_problem: Option<String>,
    /// The conflict waiting for an answer.
    conflict: Option<ConflictReport>,
    /// The per-record answers of the vault part of a conflict, in the report's
    /// order; `None` is a record not decided yet (ADR-5: nothing of the vault
    /// applies until every one is answered).
    vault_choices: Vec<(
        shellune_sync::VaultChoiceKey,
        Option<shellune_sync::VaultChoiceSide>,
    )>,
    /// The per-place answers of the data part of a conflict, in the report's
    /// order; `None` is a place not decided yet.
    document_choices: Vec<(String, Option<shellune_storage::MergeSide>)>,
    /// Whether the conflict question is on screen.
    dialog: bool,
    /// The user put the question away; automatic checks do not ask again.
    conflict_dismissed: bool,
    /// An automatic sync waited for the vault to be unlocked (ADR-4); drawn as
    /// one line on the page until the unlock (or a manual sync) clears it.
    deferred_locked: bool,
    /// How many jobs ended, for the tests.
    finished: usize,
    /// How many releases of held saves could not keep every edit (some dropped, or none
    /// written). An update that waited for the saves reads it to call itself off.
    held_lost: u64,
    /// The window's saves held back while a writer runs.
    gate: Option<DataGate>,
    /// A backup import is being applied; it shares the slot with the jobs.
    backup_running: bool,
    /// A backup import waiting for the running job.
    waiting_backup: Option<SlotTask>,
    /// The wait before the next automatic retry.
    retry_delay: Duration,
    /// Bumped by every retry timer and every success; a timer only fires for the newest.
    retry_generation: u64,
    /// The tests stop a finished job before the window takes it in, to act in between.
    #[cfg(test)]
    hold_finish: bool,
    #[cfg(test)]
    held_finish: Option<(SyncJob, Result<SyncOutcome, SyncError>)>,
}

/// The cost of sealing passwords: the default, and the cheapest Argon2 accepts in tests.
fn sync_kdf() -> KdfParams {
    if cfg!(test) {
        KdfParams::new(8, 1, 1).unwrap_or_default()
    } else {
        KdfParams::default()
    }
}

/// A platform name as a person writes it.
fn platform_label(name: &str) -> String {
    match name {
        "windows" => "Windows".to_owned(),
        "macos" => "macOS".to_owned(),
        "linux" => "Linux".to_owned(),
        other => other.to_owned(),
    }
}

/// Why a WebDAV request failed, in the window's language.
fn webdav_error_text(error: &WebDavError, strings: &SyncStrings) -> String {
    let detail = |reason: &str| copy::fill(strings.error_detail, &[("reason", reason)]);
    match error {
        WebDavError::Auth { .. } => strings.error_auth.to_owned(),
        WebDavError::Unreachable { reason } => {
            format!("{} {}", strings.error_unreachable, detail(reason))
        }
        WebDavError::NotWritable { path, .. } => {
            copy::fill(strings.error_not_writable, &[("path", path)])
        }
        WebDavError::InvalidAddress { reason } => {
            format!("{} {}", strings.error_address, detail(reason))
        }
        WebDavError::Conflict => strings.error_busy.to_owned(),
        WebDavError::Status { .. } | WebDavError::Protocol { .. } => {
            format!("{} {}", strings.error_server, detail(&error.to_string()))
        }
    }
}

/// Why a sync failed, in the window's language.
fn sync_error_text(error: &SyncError, strings: &SyncStrings) -> String {
    let detail = |reason: &str| copy::fill(strings.error_detail, &[("reason", reason)]);
    match error {
        SyncError::NotConfigured => strings.error_not_configured.to_owned(),
        SyncError::Config(error) => {
            format!("{} {}", strings.error_settings, detail(&error.to_string()))
        }
        SyncError::Credentials(error) => {
            format!(
                "{} {}",
                strings.error_credentials,
                detail(&error.to_string())
            )
        }
        SyncError::PassphraseMissing => strings.error_passphrase_missing.to_owned(),
        SyncError::Vault(error) => format!(
            "{} {}",
            strings.error_remote_document,
            detail(&error.to_string())
        ),
        SyncError::Rollback { found, applied } => copy::fill(
            strings.error_rollback,
            &[
                ("found", &found.to_string()),
                ("applied", &applied.to_string()),
            ],
        ),
        SyncError::Remote(error) => webdav_error_text(error, strings),
        SyncError::RemoteDocument(PortableError::WrongPassphrase) => {
            strings.error_remote_passphrase.to_owned()
        }
        SyncError::RemoteDocument(error) => format!(
            "{} {}",
            strings.error_remote_document,
            detail(&error.to_string())
        ),
        SyncError::NothingInCloud => strings.error_nothing_in_cloud.to_owned(),
        SyncError::Local(error) => {
            format!("{} {}", strings.error_local, detail(&error.to_string()))
        }
        SyncError::Busy => strings.error_busy.to_owned(),
    }
}

/// The width of the sync passphrase field in its settings row, before zoom.
const PASSPHRASE_FIELD_WIDTH: f32 = 240.0;

/// The engine's wording of a credential-store failure, up to its detail. The store's
/// error cannot be built outside the storage crate, so this one sentence is spelled
/// out (it is the `#[error]` text of `SyncError::Credentials`).
const CREDENTIALS_PREFIX: &str = "the sync credentials could not be read: ";

/// A failure reason kept in `sync-state.json` (the engine's English sentence) in the
/// window's language. The sentences are matched against the engine's own wording, so
/// a reason this build does not know is shown under a general sentence, as saved.
fn saved_reason_text(reason: &str, strings: &SyncStrings) -> String {
    /// Stands in for the variable part of a sentence.
    const MARK: &str = "\u{1}";
    let detail = |reason: &str| copy::fill(strings.error_detail, &[("reason", reason)]);
    // A status code is a number, so such a sentence is matched up to it.
    let without_status = |said: &str| {
        said.trim_end_matches(|c: char| c.is_ascii_digit() || c == ')')
            .to_owned()
    };
    let split = |template: String| -> (String, String) {
        match template.split_once(MARK) {
            Some((before, after)) => (before.to_owned(), after.to_owned()),
            None => (template, String::new()),
        }
    };
    let inside = |template: String| -> Option<String> {
        let (before, after) = split(template);
        reason
            .strip_prefix(&before)?
            .strip_suffix(&after)
            .map(str::to_owned)
    };

    let exact = [
        (
            SyncError::NotConfigured.to_string(),
            strings.error_not_configured,
        ),
        (
            SyncError::PassphraseMissing.to_string(),
            strings.error_passphrase_missing,
        ),
        (
            SyncError::NothingInCloud.to_string(),
            strings.error_nothing_in_cloud,
        ),
        (SyncError::Busy.to_string(), strings.error_busy),
        (WebDavError::Conflict.to_string(), strings.error_busy),
        (
            SyncError::RemoteDocument(PortableError::WrongPassphrase).to_string(),
            strings.error_remote_passphrase,
        ),
    ];
    if let Some((_, text)) = exact.iter().find(|(said, _)| said == reason) {
        return (*text).to_owned();
    }
    if let Some((found, applied)) = rollback_versions(reason) {
        return copy::fill(
            strings.error_rollback,
            &[
                ("found", &found.to_string()),
                ("applied", &applied.to_string()),
            ],
        );
    }
    if reason.starts_with(&without_status(
        &WebDavError::Auth { status: 0 }.to_string(),
    )) {
        return strings.error_auth.to_owned();
    }
    if let Some(why) = inside(
        WebDavError::Unreachable {
            reason: MARK.to_owned(),
        }
        .to_string(),
    ) {
        return format!("{} {}", strings.error_unreachable, detail(&why));
    }
    if let Some(why) = inside(
        WebDavError::InvalidAddress {
            reason: MARK.to_owned(),
        }
        .to_string(),
    ) {
        return format!("{} {}", strings.error_address, detail(&why));
    }
    let (before, after) = split(
        WebDavError::NotWritable {
            path: MARK.to_owned(),
            status: 0,
        }
        .to_string(),
    );
    if let Some((path, _)) = reason
        .strip_prefix(&before)
        .and_then(|rest| rest.split_once(without_status(&after).as_str()))
    {
        return copy::fill(strings.error_not_writable, &[("path", path)]);
    }
    let (status_before, _) = split(
        WebDavError::Status {
            method: MARK.to_owned(),
            status: 0,
            location: None,
        }
        .to_string(),
    );
    let protocol = inside(
        WebDavError::Protocol {
            reason: MARK.to_owned(),
        }
        .to_string(),
    );
    if reason.starts_with(&status_before) || protocol.is_some() {
        return format!("{} {}", strings.error_server, detail(reason));
    }
    let document = SyncError::RemoteDocument(PortableError::WrongPassphrase).to_string();
    let document_prefix = document
        .strip_suffix(&PortableError::WrongPassphrase.to_string())
        .unwrap_or(&document);
    if let Some(rest) = reason.strip_prefix(document_prefix) {
        return format!("{} {}", strings.error_remote_document, detail(rest));
    }
    if let Some(rest) = reason.strip_prefix(CREDENTIALS_PREFIX) {
        return format!("{} {}", strings.error_credentials, detail(rest));
    }
    format!("{} {}", strings.error_last_failed, detail(reason))
}

/// The two versions of a saved rollback failure (the engine's sentence), when
/// `reason` is one.
fn rollback_versions(reason: &str) -> Option<(u64, u64)> {
    let template = SyncError::Rollback {
        found: 0,
        applied: 0,
    }
    .to_string();
    let lead = template.split("(version").next()?;
    if !reason.starts_with(lead) {
        return None;
    }
    let mut numbers = reason.split("(version ").skip(1).filter_map(|part| {
        part.split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse::<u64>().ok())
    });
    Some((numbers.next()?, numbers.next()?))
}

/// A data document as the window names it.
fn document_name(label: &str, strings: &SyncStrings) -> &'static str {
    match label {
        "saved hosts" => strings.doc_hosts,
        "host groups" => strings.doc_groups,
        "forwarding rules" => strings.doc_forwards,
        "saved commands" => strings.doc_snippets,
        "trusted server identities" => strings.doc_trust,
        _ => strings.doc_preferences,
    }
}

/// The choice-table key of one vault conflict.
fn document_choices_of(
    report: &ConflictReport,
) -> Vec<(String, Option<shellune_storage::MergeSide>)> {
    if !report.per_record {
        return Vec::new();
    }
    report
        .document
        .iter()
        .map(|place| (place.key.clone(), None))
        .collect()
}

/// A field of a record as a person reads it; an unknown one as it is named.
fn field_label(field: &str, strings: &SyncStrings) -> String {
    match field {
        "name" => strings.field_name,
        "address" | "host" => strings.field_address,
        "port" => strings.field_port,
        "user" | "username" => strings.field_user,
        "group" | "group_path" => strings.field_group,
        "identity_file" => strings.field_identity_file,
        "auth" => strings.field_auth,
        "managed_key_id" => strings.field_managed_key,
        "credential_id" => strings.field_credential,
        "startup_command" => strings.field_startup,
        "jumps" => strings.field_jumps,
        "proxy_command" => strings.field_proxy,
        "keymap" => strings.field_keymap,
        other => return other.replace('_', " "),
    }
    .to_owned()
}

fn choice_key_of(conflict: &shellune_sync::VaultConflict) -> shellune_sync::VaultChoiceKey {
    match conflict {
        shellune_sync::VaultConflict::Record { kind, id, .. } => {
            shellune_sync::VaultChoiceKey::Record {
                kind: *kind,
                id: id.clone(),
            }
        }
        shellune_sync::VaultConflict::Trust { host, port, .. } => {
            shellune_sync::VaultChoiceKey::Trust {
                host: host.clone(),
                port: *port,
            }
        }
    }
}

/// Whether the touched documents of `a` and `b` differ.
fn touched_differ(a: &LocalState, b: &LocalState, touched: Touched) -> bool {
    (touched.hosts && a.hosts != b.hosts)
        || (touched.groups && a.groups != b.groups)
        || (touched.forwards && a.forwards != b.forwards)
        || (touched.snippets && a.snippets != b.snippets)
        || (touched.known_hosts && a.known_hosts != b.known_hosts)
        || (touched.preferences && a.preferences != b.preferences)
}

/// The touched documents, named the way `LocalData` names them in its errors.
fn touched_labels(touched: Touched) -> Vec<&'static str> {
    [
        (touched.hosts, "saved hosts"),
        (touched.groups, "host groups"),
        (touched.forwards, "forwarding rules"),
        (touched.snippets, "saved commands"),
        (touched.known_hosts, "trusted server identities"),
        (touched.preferences, "preferences"),
    ]
    .into_iter()
    .filter_map(|(changed, label)| changed.then_some(label))
    .collect()
}

impl SyncCenter {
    /// The sync of the data in `root`, with secrets in `secrets`. Reads the saved
    /// settings and state; sends nothing.
    pub(super) fn new(
        root: &std::path::Path,
        secrets: std::sync::Arc<dyn SecretStore>,
        locale: copy::Locale,
        cx: &mut Context<ShelluneView>,
    ) -> Self {
        let service = SyncService::new(LocalData::at(root), DataPlatform::current(), secrets)
            .with_kdf(sync_kdf());
        let config = service.load_config().ok().flatten();
        let state = service.state();
        let password_saved = matches!(service.password(), Ok(Some(_)));
        let passphrase_saved = matches!(service.passphrase(), Ok(Some(_)));
        let strings = copy::sync_for(locale);
        let shown = config.clone().unwrap_or_default();
        let url = cx.new(|cx| {
            TextField::with_text(
                "https://dav.example.com/remote.php/dav/files/me/",
                shown.url.clone(),
                cx,
            )
        });
        let username = cx.new(|cx| TextField::with_text("", shown.username.clone(), cx));
        let remote_dir = cx.new(|cx| {
            TextField::with_text(
                shellune_sync::DEFAULT_REMOTE_DIR,
                shown.remote_dir.clone(),
                cx,
            )
        });
        let secret_field = |saved: bool, cx: &mut Context<ShelluneView>| {
            cx.new(|cx| {
                let mut field = TextField::new(if saved { strings.secret_saved } else { "" }, cx);
                field.set_masked(true, cx);
                field.set_revealable(true, cx);
                field
            })
        };
        let password = secret_field(password_saved, cx);
        let passphrase = secret_field(passphrase_saved, cx);
        Self {
            service,
            draft_auto: shown.auto,
            config,
            state,
            url,
            username,
            password,
            remote_dir,
            passphrase,
            password_saved,
            passphrase_saved,
            running: None,
            queued: None,
            change_generation: 0,
            applying: false,
            polling: false,
            network: true,
            failure: None,
            test: TestStatus::Idle,
            form_problem: None,
            conflict: None,
            vault_choices: Vec::new(),
            document_choices: Vec::new(),
            dialog: false,
            conflict_dismissed: false,
            deferred_locked: false,
            finished: 0,
            held_lost: 0,
            gate: None,
            backup_running: false,
            waiting_backup: None,
            retry_delay: RETRY_FIRST,
            retry_generation: 0,
            #[cfg(test)]
            hold_finish: false,
            #[cfg(test)]
            held_finish: None,
        }
    }

    /// Whether this window may reach a server at all; a capture draws offline.
    #[must_use]
    pub(super) fn online(mut self, online: bool) -> Self {
        self.network = online;
        self
    }

    /// Whether a server is saved.
    fn configured(&self) -> bool {
        self.config.as_ref().is_some_and(SyncConfig::is_configured)
    }

    /// Whether automatic sync may run now.
    fn automatic(&self) -> bool {
        self.network
            && self
                .config
                .as_ref()
                .is_some_and(|config| config.is_configured() && config.auto)
    }

    /// Whether a conflict is waiting for an answer, in this run or from the last one.
    fn has_conflict(&self) -> bool {
        self.conflict.is_some() || self.state.has_conflict()
    }

    /// Whether the last attempt failed.
    fn has_failure(&self) -> bool {
        self.failure.is_some() || matches!(self.state.last_result, Some(LastResult::Failed { .. }))
    }
}

impl ShelluneView {
    /// The sync strings of the language on screen.
    fn sync_strings(&self) -> &'static SyncStrings {
        copy::sync_for(self.workspace.locale())
    }

    /// Starts automatic sync once the stores are loaded: a pull now and a check every
    /// [`POLL_INTERVAL`]. Without a saved server, or with automatic sync off, nothing is
    /// sent (R8).
    pub(super) fn start_sync(&mut self, cx: &mut Context<Self>) {
        if !self.sync.automatic() {
            return;
        }
        self.ensure_sync_polling(cx);
        self.run_sync_job(SyncJob::Auto, cx);
    }

    /// Whether an automatic sync would carry the vault while this device's
    /// vault is locked: the one case a job must not start unattended, because
    /// the engine's vault work reads the system credential store and could
    /// raise the system's own prompt behind the user's back (ADR-4). A manual
    /// sync is the user's own decision and always runs.
    fn automatic_sync_blocked_by_lock(&self) -> bool {
        self.sync.configured()
            && shellune_storage::VaultStore::at(self.store.root()).exists()
            && self.vault_locked()
    }

    /// Notes that data that travels was saved. With automatic sync on, an upload starts
    /// [`DEBOUNCE`] after the last such note; while cloud data is being adopted the note
    /// is dropped, so a pull is not sent back.
    pub(super) fn note_data_changed(&mut self, cx: &mut Context<Self>) {
        if self.sync.applying || !self.sync.automatic() {
            return;
        }
        self.sync.change_generation += 1;
        let generation = self.sync.change_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |view, cx| {
                if view.sync.change_generation == generation && view.sync.automatic() {
                    view.run_sync_job(SyncJob::Auto, cx);
                }
            });
        })
        .detach();
    }

    /// Checks the cloud every [`POLL_INTERVAL`] while automatic sync is on. One loop per
    /// window; it asks the switch every time it wakes.
    fn ensure_sync_polling(&mut self, cx: &mut Context<Self>) {
        if self.sync.polling || !self.sync.network {
            return;
        }
        self.sync.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let alive = this
                    .update(cx, |view, cx| {
                        if view.sync.automatic() {
                            view.run_sync_job(SyncJob::Auto, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Runs one job on the background executor, or keeps it for when the running one
    /// ends. The engine is never called twice at once.
    fn run_sync_job(&mut self, job: SyncJob, cx: &mut Context<Self>) {
        if !self.sync.network || !self.sync.configured() {
            return;
        }
        // An automatic sync never raises a system verification behind the
        // user's back: while the vault of this device is locked and the sync
        // would carry it, the job waits for the unlock (or a manual sync)
        // instead (ADR-4). It is not a failure - the next poll, change or
        // unlock picks it up again.
        if job == SyncJob::Auto && self.automatic_sync_blocked_by_lock() {
            self.sync.deferred_locked = true;
            cx.notify();
            return;
        }
        self.sync.deferred_locked = false;
        // A vault job running now saves what it opened: a sync writing the
        // vault meanwhile would lose to it, so the sync waits for it (R4).
        if self.sync_slot_busy() || self.security_job_running() {
            self.sync.queued = match self.sync.queued.take() {
                waiting @ Some(_) if job == SyncJob::Auto => waiting,
                _ => Some(job),
            };
            return;
        }
        self.sync.running = Some(job.clone());
        // Every job may write the vault (an upload adopts a legacy block into
        // it): no vault job starts until this one ended (R4).
        self.security_set_sync_writing(true);
        if job.writes_local() {
            self.hold_data_writes();
        }
        let service = self.sync.service.clone();
        let ran = job.clone();
        let work = cx.background_executor().spawn(async move {
            match ran {
                SyncJob::Auto | SyncJob::Check => service.sync(),
                SyncJob::Upload => service.upload(),
                SyncJob::Pull => service.pull(),
                SyncJob::Resolve(resolution) => service.resolve(resolution),
                SyncJob::RepairRollback => service.repair_rollback(),
                SyncJob::ResolveRecords(vault, document) => {
                    service.resolve_records(&vault, &document.into_iter().collect())
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| view.finish_sync_job(job, result, cx));
        })
        .detach();
        cx.notify();
    }

    /// Takes in what one job did, then starts the one that waited.
    fn finish_sync_job(
        &mut self,
        job: SyncJob,
        result: Result<SyncOutcome, SyncError>,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        if self.sync.hold_finish {
            self.sync.held_finish = Some((job, result));
            return;
        }
        self.sync.running = None;
        self.security_set_sync_writing(false);
        self.sync.finished += 1;
        self.sync.state = self.sync.service.state();
        // What the window saved while the job ran is merged onto what the job left on
        // disk before anything is read back.
        let held = self.release_data_writes();
        let mut adopted = false;
        if result.is_ok() {
            self.reset_sync_retry();
        }
        let strings = self.sync_strings();
        match result {
            Ok(SyncOutcome::UpToDate) => {
                self.sync_settled();
                if job.manual() {
                    self.toast(Tone::Neutral, strings.toast_up_to_date, None, cx);
                }
            }
            Ok(SyncOutcome::Uploaded(_)) => {
                self.sync_settled();
                if job.manual() {
                    self.toast(Tone::Positive, strings.toast_uploaded, None, cx);
                }
            }
            Ok(SyncOutcome::Pulled(report)) => {
                self.sync_settled();
                self.adopt_pulled_data(&report, held.preferences, strings.toast_pulled, cx);
                self.adopt_synced_vault(&report, cx);
                adopted = true;
            }
            // One side only connected to hosts: the merge was written here like a
            // pull, so it is taken in the same way, and no question is asked.
            Ok(SyncOutcome::Merged(report)) => {
                self.sync_settled();
                self.adopt_pulled_data(&report, held.preferences, strings.toast_merged, cx);
                self.adopt_synced_vault(&report, cx);
                adopted = true;
            }
            Ok(SyncOutcome::Conflict(report)) => {
                self.sync.failure = None;
                let ask = job.manual() || !self.sync.conflict_dismissed;
                self.sync.vault_choices = report
                    .vault
                    .iter()
                    .map(|conflict| (choice_key_of(conflict), None))
                    .collect();
                self.sync.document_choices = document_choices_of(&report);
                self.sync.conflict = Some(report);
                if ask {
                    self.sync.dialog = true;
                    self.sync.conflict_dismissed = false;
                }
            }
            Ok(SyncOutcome::ConflictKept) => {
                self.sync.dialog = false;
                self.sync.conflict_dismissed = true;
            }
            Err(SyncError::NotConfigured) => {}
            Err(error) => {
                let text = sync_error_text(&error, strings);
                self.sync.failure = Some(text.clone());
                if job.manual() {
                    self.toast(Tone::Danger, strings.toast_failed, Some(text), cx);
                }
                self.schedule_sync_retry(cx);
            }
        }
        self.settle_held_writes(held, adopted, cx);
        // The vault jobs that waited for this sync go now, onto what it left:
        // after the re-read of a vault it applied, which they wait for.
        self.host_secrets_flush(cx);
        self.start_next_in_sync_slot(cx);
        cx.notify();
    }

    /// Re-reads this machine's vault after a sync applied one (ADR-5): the
    /// engine wrote the file, and the window's in-memory copy of the records,
    /// the references and the trust is stale until it is opened again. A
    /// locked device stays locked - the file changed, the memory did not fill.
    fn adopt_synced_vault(&mut self, report: &PullReport, cx: &mut Context<Self>) {
        if report.vault_restored.is_none() || self.vault_locked() {
            return;
        }
        self.vault_reread(cx);
    }

    /// Starts the automatic sync an unlocked vault was waiting for (ADR-4).
    /// Called from the unlock itself, so the deferral ends the moment the
    /// vault opens again rather than at the next poll.
    pub(super) fn resume_deferred_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync.deferred_locked
            && self.sync.automatic()
            && !self.automatic_sync_blocked_by_lock()
        {
            self.run_sync_job(SyncJob::Auto, cx);
        }
    }

    /// The key one vault conflict is answered by, for the choice table.
    fn choose_vault_side(
        &mut self,
        index: usize,
        side: shellune_sync::VaultChoiceSide,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, chosen)) = self.sync.vault_choices.get_mut(index) {
            *chosen = Some(side);
        }
        cx.notify();
    }

    /// Answers one place of the data part of a conflict.
    fn choose_document_side(
        &mut self,
        index: usize,
        side: shellune_storage::MergeSide,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, chosen)) = self.sync.document_choices.get_mut(index) {
            *chosen = Some(side);
        }
        cx.notify();
    }

    /// Answers every place of the conflict, vault records included, with one
    /// side: the quick way through a long list, still applied as one.
    fn choose_every_side(&mut self, local: bool, cx: &mut Context<Self>) {
        let (vault, document) = if local {
            (
                shellune_sync::VaultChoiceSide::Local,
                shellune_storage::MergeSide::Local,
            )
        } else {
            (
                shellune_sync::VaultChoiceSide::Remote,
                shellune_storage::MergeSide::Remote,
            )
        };
        for (_, chosen) in &mut self.sync.vault_choices {
            *chosen = Some(vault);
        }
        for (_, chosen) in &mut self.sync.document_choices {
            *chosen = Some(document);
        }
        cx.notify();
    }

    /// Sends the per-place answers of a conflict: the vault records (ADR-5)
    /// and the data's records and settings. Every place decided is the
    /// engine's own precondition; the button that lands here is only drawn
    /// when the last open one was answered.
    pub(super) fn resolve_sync_vault(&mut self, cx: &mut Context<Self>) {
        let document: Vec<(String, shellune_storage::MergeSide)> = self
            .sync
            .document_choices
            .iter()
            .filter_map(|(key, side)| side.map(|side| (key.clone(), side)))
            .collect();
        let choices: Vec<shellune_sync::VaultChoice> = self
            .sync
            .vault_choices
            .iter()
            .filter_map(|(key, side)| {
                side.map(|side| shellune_sync::VaultChoice {
                    key: key.clone(),
                    side,
                })
            })
            .collect();
        self.sync.dialog = false;
        self.run_sync_job(SyncJob::ResolveRecords(choices, document), cx);
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // The slot and the saves held back while a writer runs
    // -----------------------------------------------------------------------

    /// Whether a job or a backup import holds the slot.
    pub(super) fn sync_slot_busy(&self) -> bool {
        self.sync.running.is_some() || self.sync.backup_running
    }

    /// Starts what waited for the slot: a backup import first (a person is watching
    /// it), then the queued job.
    pub(super) fn start_next_in_sync_slot(&mut self, cx: &mut Context<Self>) {
        if self.sync_slot_busy() {
            return;
        }
        // A close that waited for the held saves happens now that they are written;
        // nothing else is started for a window that is going away.
        if self.finish_exit_after_held_writes(cx) {
            return;
        }
        if let Some(task) = self.sync.waiting_backup.take() {
            task(self, cx);
            return;
        }
        if let Some(next) = self.sync.queued.take() {
            self.run_sync_job(next, cx);
        }
    }

    /// Starts the sync that waited for a vault job, once the job ended and
    /// nothing else holds the slot.
    pub(super) fn resume_sync_after_vault_job(&mut self, cx: &mut Context<Self>) {
        if self.sync_slot_busy() || self.security_job_running() {
            return;
        }
        if let Some(next) = self.sync.queued.take() {
            self.run_sync_job(next, cx);
        }
    }

    /// Runs `task` (a backup import) when the slot is free: now, or when the running
    /// job ends. A later request replaces one that still waits.
    pub(super) fn when_sync_slot_free(&mut self, task: SlotTask, cx: &mut Context<Self>) {
        if self.sync_slot_busy() {
            self.sync.waiting_backup = Some(task);
        } else {
            task(self, cx);
        }
    }

    /// Marks the slot as held by a backup import.
    pub(super) fn set_backup_in_sync_slot(&mut self, running: bool) {
        self.sync.backup_running = running;
    }

    /// Whether the window's saves are held back now.
    pub(super) fn data_writes_held(&self) -> bool {
        self.sync.gate.is_some()
    }

    /// How many releases of held saves so far could not keep every edit. A caller that
    /// waited for the saves compares it with the count from before it waited.
    pub(super) fn held_writes_lost_count(&self) -> u64 {
        self.sync.held_lost
    }

    /// Whether saves the window already showed are held back and not yet on disk.
    pub(super) fn held_writes_pending(&self) -> bool {
        self.sync
            .gate
            .as_ref()
            .is_some_and(|gate| gate.touched.any() || self.snippets.has_held_writes())
    }

    /// Keeps a close or quit back while saves are held: it happens once the writer is
    /// done and they are written (bounded by the engine's timeouts). Returns whether
    /// the exit has to wait.
    pub(super) fn defer_exit_for_held_writes(
        &mut self,
        exit: PendingExit,
        cx: &mut Context<Self>,
    ) -> bool {
        // A form save that waits for the running writer is held back like the
        // saves above.
        let form_waits = self.host_form_save_in_flight() && self.sync_slot_busy();
        if !self.held_writes_pending() && !form_waits {
            return false;
        }
        let first = self.exit_after_held_writes.is_none();
        // A quit includes closing the window, so it is never downgraded.
        if self.exit_after_held_writes != Some(PendingExit::Quit) {
            self.exit_after_held_writes = Some(exit);
        }
        if first {
            let strings = self.sync_strings();
            self.toast(
                Tone::Neutral,
                strings.exit_waits,
                Some(strings.exit_waits_detail.to_owned()),
                cx,
            );
        }
        cx.notify();
        true
    }

    /// Closes the window, or quits, when that waited for the held saves and they are
    /// written now. Returns whether the exit was carried out.
    fn finish_exit_after_held_writes(&mut self, cx: &mut Context<Self>) -> bool {
        // A form whose password is on its way to the vault writes its record
        // first; its finish carries the exit out.
        if self.sync.gate.is_some() || self.host_form_save_in_flight() {
            return false;
        }
        let Some(exit) = self.exit_after_held_writes.take() else {
            return false;
        };
        // Work that started while the close waited is asked about like any other.
        if !self.app_close_confirmed && (self.transfers.any_active() || self.forwards.any_active())
        {
            self.closing_app = true;
            self.terminate_pending = exit == PendingExit::Quit;
            cx.notify();
            return false;
        }
        self.app_close_confirmed = true;
        match exit {
            PendingExit::Quit => cx.quit(),
            PendingExit::Window => {
                if let Some(handle) = self.own_window {
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    });
                }
            }
        }
        true
    }

    /// Carries out a close that waited for a form's save, now that the save
    /// ended: when it was written and nothing else holds the exit, or calls
    /// the close off when it was not, so the reason can be read.
    pub(super) fn exit_after_form_save(&mut self, saved: bool, cx: &mut Context<Self>) {
        if self.exit_after_held_writes.is_none() {
            return;
        }
        if !saved {
            self.exit_after_held_writes = None;
            cx.notify();
            return;
        }
        if self.sync_slot_busy() {
            return;
        }
        self.finish_exit_after_held_writes(cx);
    }

    /// Starts holding the window's saves back, remembering the data as the window
    /// holds it now (no file is read on the way).
    pub(super) fn hold_data_writes(&mut self) {
        if self.sync.gate.is_some() {
            return;
        }
        let base = Box::new(self.data_in_memory());
        self.snippets.hold_writes();
        self.sync.gate = Some(DataGate {
            base,
            touched: Touched::default(),
        });
    }

    /// The data files as the window holds them now.
    fn data_in_memory(&self) -> LocalState {
        LocalState {
            hosts: self.book.clone(),
            groups: self.groups.clone(),
            forwards: self.forwards.book().clone(),
            snippets: self.snippets.book().clone(),
            known_hosts: self.trust_book.clone(),
            preferences: self.saved_preferences.clone().unwrap_or_default(),
        }
    }

    /// Stops holding saves back and writes what was held, merged onto the files as the
    /// writer left them. Nothing the writer put there is overwritten by a stale copy.
    pub(super) fn release_data_writes(&mut self) -> HeldWrites {
        let Some(mut gate) = self.sync.gate.take() else {
            return HeldWrites::default();
        };
        if self.snippets.release_writes() {
            gate.touched.snippets = true;
        }
        let touched = gate.touched;
        if !touched.any() {
            return HeldWrites::default();
        }
        let ours = self.data_in_memory();
        let data = LocalData::at(self.store.root());
        match data.rebase(&gate.base, &ours, touched) {
            Ok(rebased) => HeldWrites {
                written: rebased.written,
                reload: touched_differ(&rebased.state, &ours, touched),
                preferences: touched.preferences && rebased.state.preferences != ours.preferences,
                dropped: rebased.dropped,
                failed: None,
            },
            // The files could not be read (or one could not be written). The window's
            // copy is never written whole over them - that could drop what the writer
            // put there - so the files stay as they are, the window reads them again,
            // and the user is told which changes to make again.
            Err(error) => HeldWrites {
                written: false,
                reload: true,
                preferences: touched.preferences,
                dropped: touched_labels(touched),
                failed: Some(error.to_string()),
            },
        }
    }

    /// Finishes what releasing the held saves started: reads back what differs from the
    /// screen, notes the written change for upload, and says what could not be kept.
    pub(super) fn settle_held_writes(
        &mut self,
        held: HeldWrites,
        adopted: bool,
        cx: &mut Context<Self>,
    ) {
        if held.reload && !adopted {
            self.adopt_imported_data(held.preferences, cx);
        }
        if held.written {
            self.note_data_changed(cx);
        }
        let strings = self.sync_strings();
        if !held.dropped.is_empty() {
            let names: Vec<&str> = held
                .dropped
                .iter()
                .map(|label| document_name(label, strings))
                .collect();
            self.toast(
                Tone::Warning,
                strings.held_dropped,
                Some(copy::fill(
                    strings.held_dropped_detail,
                    &[("list", &names.join(", "))],
                )),
                cx,
            );
        }
        if let Some(reason) = &held.failed {
            self.toast(
                Tone::Danger,
                strings.held_failed,
                Some(copy::fill(strings.error_detail, &[("reason", reason)])),
                cx,
            );
        }
        if !held.dropped.is_empty() || held.failed.is_some() {
            self.sync.held_lost += 1;
        }
        // A close that waited for these saves does not hide that some were not kept:
        // the window stays so the notices above can be read.
        if (!held.dropped.is_empty() || held.failed.is_some())
            && self.exit_after_held_writes.take().is_some()
        {
            self.toast(Tone::Warning, strings.exit_stopped, None, cx);
        }
    }

    /// Writes the host book, or holds the write back while a writer runs.
    pub(super) fn write_hosts_file(&mut self, book: &HostBook) -> Result<(), StorageError> {
        if let Some(gate) = self.sync.gate.as_mut() {
            gate.touched.hosts = true;
            return Ok(());
        }
        self.store.save(book)
    }

    /// Writes the trusted identities the window holds, or holds the write back.
    pub(super) fn write_trust_file(&mut self) -> Result<(), StorageError> {
        if let Some(gate) = self.sync.gate.as_mut() {
            gate.touched.known_hosts = true;
            return Ok(());
        }
        self.trust.save(&self.trust_book)
    }

    /// Writes the forwarding rules, or holds the write back.
    pub(super) fn write_forwards_file(&mut self, book: &ForwardBook) -> Result<(), PersistError> {
        if let Some(gate) = self.sync.gate.as_mut() {
            gate.touched.forwards = true;
            return Ok(());
        }
        ForwardStore::at(self.store.root()).save(book)
    }

    /// Writes the preferences, or holds the write back.
    pub(super) fn write_preferences_file(
        &mut self,
        preferences: &Preferences,
    ) -> Result<(), PersistError> {
        if let Some(gate) = self.sync.gate.as_mut() {
            gate.touched.preferences = true;
            return Ok(());
        }
        PreferencesStore::at(self.store.root()).save(preferences)
    }

    /// The group service for one change: the real one, or - while writes are held
    /// back - one over a scratch directory, so the change is computed and checked the
    /// same way without touching the files a writer may have just replaced.
    pub(super) fn group_writer(&mut self) -> GroupWriter {
        if self.sync.gate.is_none() {
            return GroupWriter {
                service: self.group_service.clone(),
                scratch: None,
            };
        }
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let scratch = self.store.root().join(format!(
            ".held-group-change-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        GroupWriter {
            service: GroupService::at(&scratch),
            scratch: Some(scratch),
        }
    }

    /// Ends a group change: a held one marks the hosts and the tree as changed, and
    /// its scratch directory is removed.
    pub(super) fn finish_group_write(&mut self, writer: GroupWriter) {
        let Some(scratch) = writer.scratch else {
            return;
        };
        let _ = std::fs::remove_dir_all(&scratch);
        if let Some(gate) = self.sync.gate.as_mut() {
            gate.touched.hosts = true;
            gate.touched.groups = true;
        }
    }

    /// After a failure, tries again in a while: [`RETRY_FIRST`], then twice as long
    /// each time up to [`RETRY_MAX`]. Only while automatic sync is on.
    fn schedule_sync_retry(&mut self, cx: &mut Context<Self>) {
        if !self.sync.automatic() {
            return;
        }
        let delay = self.sync.retry_delay;
        self.sync.retry_delay = (delay * 2).min(RETRY_MAX);
        self.sync.retry_generation += 1;
        let generation = self.sync.retry_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |view, cx| {
                if view.sync.retry_generation == generation
                    && view.sync.automatic()
                    && view.sync.has_failure()
                {
                    view.run_sync_job(SyncJob::Auto, cx);
                }
            });
        })
        .detach();
    }

    /// A sync got through: the next failure waits [`RETRY_FIRST`] again, and a retry
    /// still waiting is dropped.
    fn reset_sync_retry(&mut self) {
        self.sync.retry_delay = RETRY_FIRST;
        self.sync.retry_generation += 1;
    }

    /// Both sides agree again: no failure and no conflict.
    fn sync_settled(&mut self) {
        self.sync.failure = None;
        self.sync.conflict = None;
        self.sync.dialog = false;
        self.sync.conflict_dismissed = false;
    }

    /// Shows what a pull wrote: every list and preference is read again, and the notes
    /// about it are one notification.
    fn adopt_pulled_data(
        &mut self,
        report: &PullReport,
        preferences: bool,
        title: &'static str,
        cx: &mut Context<Self>,
    ) {
        let strings = self.sync_strings();
        self.sync.applying = true;
        self.adopt_imported_data(
            preferences || report.preferences_changed || report.keymap_changed,
            cx,
        );
        self.key_presence.forget();
        self.sync.applying = false;
        if !report.secrets_to_forget.is_empty() {
            // The vault took the deletions; an old entry of those hosts that
            // is still on this machine goes with them (R1).
            self.host_legacy_forget(report.secrets_to_forget.clone(), cx);
        }
        let mut notes = Vec::new();
        if let Some(restored) = &report.vault_restored {
            notes.push(copy::fill(
                strings.pull_vault,
                &[("version", &restored.global_counter.to_string())],
            ));
        }
        if !report.trust_conflicts.is_empty() {
            notes.push(copy::fill(
                strings.pull_trust,
                &[("count", &report.trust_conflicts.len().to_string())],
            ));
        }
        if !report.secrets_not_forgotten.is_empty() {
            notes.push(copy::fill(
                strings.pull_not_forgotten,
                &[("count", &report.secrets_not_forgotten.len().to_string())],
            ));
        }
        let tone = if notes.is_empty() {
            Tone::Positive
        } else {
            Tone::Warning
        };
        self.toast(
            tone,
            title,
            (!notes.is_empty()).then(|| notes.join(" ")),
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // The settings form
    // -----------------------------------------------------------------------

    /// The settings the form describes now.
    fn sync_draft(&self, cx: &gpui::App) -> SyncConfig {
        let remote_dir = self.sync.remote_dir.read(cx).text().trim().to_owned();
        SyncConfig {
            url: self.sync.url.read(cx).text().trim().to_owned(),
            username: self.sync.username.read(cx).text().trim().to_owned(),
            remote_dir: if remote_dir.is_empty() {
                shellune_sync::DEFAULT_REMOTE_DIR.to_owned()
            } else {
                remote_dir
            },
            auto: self.sync.draft_auto,
        }
    }

    /// Saves the form: the settings to `sync.json`, the password and the passphrase to
    /// the credential store (an empty field keeps the saved one). With automatic sync on
    /// the first sync starts at once.
    pub(super) fn save_sync_settings(&mut self, cx: &mut Context<Self>) {
        let strings = self.sync_strings();
        let draft = self.sync_draft(cx);
        let password = self.sync.password.read(cx).text().to_owned();
        let passphrase = self.sync.passphrase.read(cx).text().to_owned();
        // The vault travels under the WebDAV password when there is one, so
        // that mode needs no second secret; a passwordless WebDAV must have
        // the sync passphrase before the vault may travel at all (R5).
        let webdav_password = !password.trim().is_empty()
            || self
                .sync
                .service
                .password()
                .ok()
                .flatten()
                .is_some_and(|saved| !saved.expose().trim().is_empty());
        if !webdav_password && !self.sync.passphrase_saved && passphrase.trim().is_empty() {
            self.sync.form_problem = Some(strings.passphrase_required.to_owned());
            cx.notify();
            return;
        }
        let service = self.sync.service.clone();
        let saved = (|| {
            service
                .save_config(&draft)
                .map_err(|error| sync_error_text(&SyncError::Config(error), strings))?;
            if !password.is_empty() {
                service
                    .set_password(Some(&SecretString::new(password.clone())))
                    .map_err(|error| sync_error_text(&SyncError::Credentials(error), strings))?;
            }
            if !passphrase.trim().is_empty() {
                service
                    .set_passphrase(Some(&SecretString::new(passphrase.clone())))
                    .map_err(|error| sync_error_text(&SyncError::Credentials(error), strings))?;
            }
            Ok::<(), String>(())
        })();
        match saved {
            Ok(()) => {
                self.sync.form_problem = None;
                if !password.is_empty() {
                    self.sync.password_saved = true;
                }
                if !passphrase.trim().is_empty() {
                    self.sync.passphrase_saved = true;
                }
                self.sync.config = Some(draft);
                self.sync.state = self.sync.service.state();
                // A secret is not kept on screen once it is in the store.
                let placeholder = strings.secret_saved;
                for (field, saved) in [
                    (self.sync.password.clone(), self.sync.password_saved),
                    (self.sync.passphrase.clone(), self.sync.passphrase_saved),
                ] {
                    field.update(cx, |field, cx| {
                        field.set_text(String::new(), cx);
                        if saved {
                            field.set_placeholder(placeholder, cx);
                        }
                    });
                }
                self.toast(Tone::Positive, strings.saved, None, cx);
                if self.sync.automatic() {
                    self.ensure_sync_polling(cx);
                    self.run_sync_job(SyncJob::Auto, cx);
                }
            }
            Err(reason) => {
                self.sync.form_problem = Some(format!("{}. {reason}", strings.save_failed));
            }
        }
        cx.notify();
    }

    /// Turns automatic sync on or off. With a server saved the choice is saved at once;
    /// turning it on starts a sync.
    pub(super) fn set_sync_auto(&mut self, on: bool, cx: &mut Context<Self>) {
        self.sync.draft_auto = on;
        if let Some(mut config) = self.sync.config.clone() {
            config.auto = on;
            match self.sync.service.save_config(&config) {
                Ok(()) => {
                    self.sync.config = Some(config);
                    if self.sync.automatic() {
                        self.ensure_sync_polling(cx);
                        self.run_sync_job(SyncJob::Auto, cx);
                    }
                }
                Err(error) => {
                    self.sync.form_problem = Some(sync_error_text(
                        &SyncError::Config(error),
                        self.sync_strings(),
                    ));
                }
            }
        }
        cx.notify();
    }

    /// Tests the settings in the form - saved or not - on the background executor:
    /// reaches the server, signs in and writes a probe into the folder (R7).
    pub(super) fn test_sync_connection(&mut self, cx: &mut Context<Self>) {
        if self.sync.test == TestStatus::Running || !self.sync.network {
            return;
        }
        let strings = self.sync_strings();
        let draft = self.sync_draft(cx);
        if !draft.is_configured() {
            self.sync.test = TestStatus::Failed(strings.url_required.to_owned());
            cx.notify();
            return;
        }
        let typed = self.sync.password.read(cx).text().to_owned();
        let service = self.sync.service.clone();
        self.sync.test = TestStatus::Running;
        let work = cx.background_executor().spawn(async move {
            let password = if typed.is_empty() {
                service.password().ok().flatten()
            } else {
                Some(SecretString::new(typed))
            };
            SyncService::test_connection(&draft, password.as_ref())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                let strings = view.sync_strings();
                view.sync.test = match result {
                    Ok(()) => TestStatus::Passed,
                    Err(error) => TestStatus::Failed(webdav_error_text(&error, strings)),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Runs a sync the user asked for: upload, pull, or a check that asks about a
    /// conflict again. Works with automatic sync off too (R8).
    pub(super) fn request_sync(&mut self, job: SyncJob, cx: &mut Context<Self>) {
        if !self.sync.configured() {
            self.sync.form_problem = Some(self.sync_strings().not_configured.to_owned());
            cx.notify();
            return;
        }
        self.sync.form_problem = None;
        self.run_sync_job(job, cx);
        cx.notify();
    }

    /// Opens the conflict question again; without the details of this run, a check
    /// finds the conflict and asks.
    pub(super) fn reopen_sync_conflict(&mut self, cx: &mut Context<Self>) {
        if self.sync.conflict.is_some() {
            self.sync.dialog = true;
            self.sync.conflict_dismissed = false;
            cx.notify();
        } else {
            self.request_sync(SyncJob::Check, cx);
        }
    }

    /// Answers the conflict question. Cancel changes nothing and keeps the conflict.
    pub(super) fn resolve_sync_conflict(&mut self, resolution: Resolution, cx: &mut Context<Self>) {
        self.sync.dialog = false;
        if resolution == Resolution::Cancel {
            self.sync.conflict_dismissed = true;
            self.restore_focus_pending = true;
        }
        self.run_sync_job(SyncJob::Resolve(resolution), cx);
        cx.notify();
    }

    /// Whether the conflict question is on screen.
    pub(super) fn sync_dialog_open(&self) -> bool {
        self.sync.dialog && self.sync.conflict.is_some()
    }

    /// Opens the sync category of the settings.
    pub(super) fn open_sync_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select(Destination::Settings, window, cx);
        self.settings_select_category(Category::Sync, cx);
    }

    /// The saved sync passphrase, when a non-empty one is in the store (R9):
    /// the one secret a vault backup and a passwordless WebDAV share.
    pub(super) fn saved_sync_passphrase(&self) -> Option<SecretString> {
        self.sync
            .service
            .passphrase()
            .ok()
            .flatten()
            .filter(|passphrase| !passphrase.expose().trim().is_empty())
    }

    /// Saves the sync passphrase a backup export was sealed with, so the one
    /// secret a user sets is the one every other device uses too (R9: no
    /// third secret beside the WebDAV password and this one).
    pub(super) fn save_sync_passphrase(&self, passphrase: &SecretString) -> Result<(), String> {
        self.sync
            .service
            .set_passphrase(Some(passphrase))
            .map_err(|error| sync_error_text(&SyncError::Credentials(error), self.sync_strings()))
    }

    /// The password fields follow the language on screen.
    pub(super) fn relabel_sync_fields(&self, cx: &mut Context<Self>) {
        let strings = self.sync_strings();
        for (field, saved) in [
            (&self.sync.password, self.sync.password_saved),
            (&self.sync.passphrase, self.sync.passphrase_saved),
        ] {
            let label = if saved { strings.secret_saved } else { "" };
            field.update(cx, |field, cx| field.set_placeholder(label, cx));
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// One field of the sync page under its label.
    fn sync_field_row(
        &self,
        id: &'static str,
        label: &'static str,
        hint: &'static str,
        field: &Entity<TextField>,
    ) -> Div {
        let metrics = self.metrics();
        let selector = match id {
            "sync-url" => "settings.sync.url",
            "sync-username" => "settings.sync.username",
            "sync-password" => "settings.sync.password",
            "sync-remote-dir" => "settings.sync.remote-dir",
            _ => "settings.sync.passphrase",
        };
        self.settings_row(
            id,
            label,
            hint,
            div()
                .w(metrics.scale(PASSPHRASE_FIELD_WIDTH))
                .max_w(components::settings_row_content_cap(metrics))
                .debug_selector(move || selector.to_owned())
                .child(field.clone()),
        )
    }

    /// A line of text in the sync card.
    fn sync_line(
        &self,
        text: impl Into<SharedString>,
        tone: Option<Tone>,
        selector: &'static str,
    ) -> Div {
        let metrics = self.metrics();
        div()
            .w_full()
            .min_w_0()
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::TIGHT))
            .whitespace_normal()
            .text_size(metrics.caption_size())
            .text_color(match tone {
                Some(tone) => tone.text(),
                None => theme::swatch(palette::TEXT_SECONDARY),
            })
            .debug_selector(move || selector.to_owned())
            .child(text.into())
    }

    /// The sync status as one sentence: running, never, or the last time and result.
    fn sync_status_text(&self) -> (String, Option<Tone>) {
        let strings = self.sync_strings();
        if !self.sync.configured() {
            return (strings.status_not_configured.to_owned(), None);
        }
        if self.sync.running.is_some() {
            return (strings.status_running.to_owned(), None);
        }
        let state = &self.sync.state;
        let Some(result) = &state.last_result else {
            return (strings.status_never.to_owned(), None);
        };
        let (said, tone) = match result {
            LastResult::UpToDate => (strings.result_up_to_date.to_owned(), Some(Tone::Positive)),
            LastResult::Uploaded => (strings.result_uploaded.to_owned(), Some(Tone::Positive)),
            LastResult::Pulled => (strings.result_pulled.to_owned(), Some(Tone::Positive)),
            LastResult::Conflict => (strings.result_conflict.to_owned(), Some(Tone::Warning)),
            LastResult::Failed { reason } => (
                copy::fill(
                    strings.result_failed,
                    &[(
                        "reason",
                        &self
                            .sync
                            .failure
                            .clone()
                            .unwrap_or_else(|| saved_reason_text(reason, strings)),
                    )],
                ),
                Some(Tone::Danger),
            ),
        };
        let when = state
            .last_time_unix
            .map(crate::app::hosts::format_unix)
            .unwrap_or_default();
        (
            format!("{} {when} \u{b7} {said}", strings.status_label),
            tone,
        )
    }

    /// The sync page: the server, the switches, the actions and the status (R7, R8).
    pub(super) fn sync_settings_page(&self, cx: &mut Context<Self>) -> Div {
        let strings = self.sync_strings();
        let metrics = self.metrics();
        let switch = |view: &Self,
                      id: &'static str,
                      on: bool,
                      cx: &mut Context<Self>,
                      pick: fn(&mut Self, bool, &mut Context<Self>)| {
            view.switch_control(id, on, cx, pick)
        };
        let mut rows = vec![
            self.sync_field_row("sync-url", strings.url, strings.url_hint, &self.sync.url),
            self.sync_field_row("sync-username", strings.username, "", &self.sync.username),
            self.sync_field_row(
                "sync-password",
                strings.password,
                strings.password_hint,
                &self.sync.password,
            ),
            self.sync_field_row(
                "sync-remote-dir",
                strings.remote_dir,
                strings.remote_dir_hint,
                &self.sync.remote_dir,
            ),
            self.settings_row(
                "sync-auto",
                strings.auto,
                strings.auto_hint,
                switch(
                    self,
                    "settings.sync.auto",
                    self.sync.draft_auto,
                    cx,
                    Self::set_sync_auto,
                ),
            ),
            self.sync_field_row(
                "sync-passphrase",
                strings.passphrase,
                strings.passphrase_hint,
                &self.sync.passphrase,
            ),
        ];
        let unsaved = {
            let draft = self.sync_draft(cx);
            let typed_password = !self.sync.password.read(cx).text().is_empty();
            let typed_passphrase = !self.sync.passphrase.read(cx).text().trim().is_empty();
            match &self.sync.config {
                Some(saved) => {
                    draft.url != saved.url
                        || draft.username != saved.username
                        || draft.remote_dir != saved.remote_dir
                        || typed_password
                        || typed_passphrase
                }
                None => {
                    !draft.url.is_empty()
                        || !draft.username.is_empty()
                        || typed_password
                        || typed_passphrase
                }
            }
        };
        if unsaved {
            rows.push(self.sync_line(
                copy::ux().sync_unsaved_changes,
                Some(Tone::Warning),
                "settings.sync.unsaved",
            ));
        }
        let testing = self.sync.test == TestStatus::Running;
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::SNUG))
            .debug_selector(|| "settings.sync.actions".to_owned())
            .child(self.text_button(
                SharedString::from("settings.sync.save"),
                strings.save,
                Variant::Primary,
                cx,
                |this, cx| this.save_sync_settings(cx),
            ))
            .child(self.text_button(
                SharedString::from("settings.sync.test"),
                if testing {
                    strings.testing
                } else {
                    strings.test
                },
                Variant::Secondary,
                cx,
                |this, cx| this.test_sync_connection(cx),
            ))
            .child(self.text_button(
                SharedString::from("settings.sync.upload"),
                strings.upload,
                Variant::Secondary,
                cx,
                |this, cx| this.request_sync(SyncJob::Upload, cx),
            ))
            .child(self.text_button(
                SharedString::from("settings.sync.pull"),
                strings.pull,
                Variant::Secondary,
                cx,
                |this, cx| this.request_sync(SyncJob::Pull, cx),
            ));
        if self.sync.has_conflict() {
            actions = actions.child(self.text_button(
                SharedString::from("settings.sync.conflict"),
                strings.resolve,
                Variant::Danger,
                cx,
                |this, cx| this.reopen_sync_conflict(cx),
            ));
        }
        rows.push(actions);
        match &self.sync.test {
            TestStatus::Idle | TestStatus::Running => {}
            TestStatus::Passed => rows.push(self.sync_line(
                strings.test_ok,
                Some(Tone::Positive),
                "settings.sync.test.result",
            )),
            TestStatus::Failed(reason) => rows.push(self.sync_line(
                format!("{} {reason}", strings.test_failed),
                Some(Tone::Danger),
                "settings.sync.test.result",
            )),
        }
        if let Some(problem) = &self.sync.form_problem {
            rows.push(self.sync_line(problem.clone(), Some(Tone::Danger), "settings.sync.problem"));
        }
        let (status, tone) = self.sync_status_text();
        rows.push(
            self.sync_line(status, tone, "settings.sync.status")
                .font_weight(FontWeight::MEDIUM),
        );
        let rollback = self.sync_rollback_pending();
        if rollback && self.sync.running.is_none() {
            // Retrying never mends a rolled-back cloud; the repair does.
            rows.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::TIGHT))
                    .px(metrics.scale(spacing::LOOSE))
                    .py(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "settings.sync.repair.row".to_owned())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(strings.repair_hint),
                    )
                    .child(self.text_button(
                        SharedString::from("settings.sync.repair"),
                        strings.repair_button,
                        Variant::Primary,
                        cx,
                        |this, cx| this.request_sync(SyncJob::RepairRollback, cx),
                    )),
            );
        } else if self.sync.configured() && self.sync.has_failure() && self.sync.running.is_none() {
            rows.push(self.sync_line(strings.retry_note, None, "settings.sync.retry"));
        }
        if self.sync.deferred_locked {
            rows.push(self.sync_line(
                strings.deferred_locked,
                Some(Tone::Neutral),
                "settings.sync.deferred",
            ));
        }
        self.settings_card("sync", rows)
    }

    /// Whether the last sync stopped at a cloud vault older than this
    /// computer's, which only the merge repair settles.
    pub(super) fn sync_rollback_pending(&self) -> bool {
        matches!(
            &self.sync.state.last_result,
            Some(LastResult::Failed { reason }) if rollback_versions(reason).is_some()
        )
    }

    /// What the top row says while the last sync failed or a conflict waits: the
    /// marker's word and tone.
    pub(super) fn sync_notice(&self) -> Option<(&'static str, Tone)> {
        if !self.sync.configured() || self.sync.running.is_some() {
            return None;
        }
        let strings = self.sync_strings();
        if self.sync.has_conflict() {
            Some((strings.marker_conflict, Tone::Warning))
        } else if self.sync.has_failure() {
            Some((strings.marker_failed, Tone::Danger))
        } else {
            None
        }
    }

    /// The marker in the top bar while the last sync failed or a conflict waits; a press
    /// opens the sync settings.
    ///
    /// Its form is the row's ([`ShelluneView::topbar_notice_form`]): the word, an icon in
    /// the marker's tone with the word in its tooltip, or - in a compact row - nothing of
    /// its own, the settings button carrying it (R2, R9).
    pub(super) fn sync_topbar_marker(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let (label, tone) = self.sync_notice()?;
        let strings = self.sync_strings();
        let metrics = self.metrics();
        let open = cx.listener(|this, _event: &gpui::ClickEvent, window, cx| {
            this.open_sync_settings(window, cx);
        });
        match self.topbar_notice_form() {
            NoticeForm::Folded => None,
            NoticeForm::Icon => Some(
                notice_icon(
                    "topbar.sync",
                    Icon::Refresh,
                    tone,
                    format!("{label} \u{b7} {}", strings.marker_hint),
                    metrics,
                )
                .on_click(open),
            ),
            NoticeForm::Words => Some(
                notice_words("topbar.sync", label, tone, metrics)
                    .tooltip(components::tooltip(SharedString::from(strings.marker_hint)))
                    .on_click(open),
            ),
        }
    }

    /// The conflict question over the window (R10), and the per-record vault
    /// question that comes first when the conflict has vault records in it
    /// (ADR-5): nothing of the vault applies until every record is answered,
    /// and each answer is the user's own - never "the cloud is newer".
    pub(super) fn sync_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.sync_dialog_open() {
            return None;
        }
        let report = self.sync.conflict.as_ref()?;
        if !report.vault.is_empty() || (report.per_record && !report.document.is_empty()) {
            return Some(self.vault_conflict_overlay(report, window, cx));
        }
        let strings = self.sync_strings();
        let metrics = self.metrics();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Refresh),
            strings.conflict_title.to_owned(),
            None,
            "sync.conflict.close",
            cx,
            |this, cx| this.resolve_sync_conflict(Resolution::Cancel, cx),
        );
        let source = copy::fill(
            strings.conflict_source,
            &[
                ("platform", &platform_label(&report.remote_platform)),
                ("version", &report.remote_app_version),
                (
                    "date",
                    &crate::app::hosts::format_unix(report.remote_created_unix),
                ),
            ],
        );
        let paragraph = |text: &'static str, selector: &'static str| {
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(move || selector.to_owned())
                .child(text)
        };
        let body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "sync.conflict.body".to_owned())
            .child(
                components::banner(Tone::Warning, metrics)
                    .whitespace_normal()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "sync.conflict.intro".to_owned())
                    .child(if report.first_sync {
                        strings.conflict_first
                    } else {
                        strings.conflict_intro
                    }),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "sync.conflict.source".to_owned())
                    .child(SharedString::from(source)),
            )
            .children(
                (!report.document.is_empty())
                    .then(|| self.document_places_preview(report, metrics)),
            )
            .child(paragraph(
                strings.conflict_remote_hint,
                "sync.conflict.remote.hint",
            ))
            .child(paragraph(
                strings.conflict_local_hint,
                "sync.conflict.local.hint",
            ))
            .child(paragraph(
                strings.conflict_cancel_hint,
                "sync.conflict.cancel.hint",
            ))
            .capture_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.resolve_sync_conflict(Resolution::Cancel, cx);
                    }
                }),
            );
        let footer = components::modal_footer(metrics)
            .child(self.text_button(
                SharedString::from("sync.conflict.cancel"),
                strings.cancel,
                Variant::Ghost,
                cx,
                |this, cx| this.resolve_sync_conflict(Resolution::Cancel, cx),
            ))
            .child(self.text_button(
                SharedString::from("sync.conflict.local"),
                strings.use_local,
                Variant::Secondary,
                cx,
                |this, cx| this.resolve_sync_conflict(Resolution::UseLocal, cx),
            ))
            .child(self.text_button(
                SharedString::from("sync.conflict.remote"),
                strings.use_remote,
                Variant::Primary,
                cx,
                |this, cx| this.resolve_sync_conflict(Resolution::UseRemote, cx),
            ));
        Some(self.modal(
            window,
            "sync.conflict.dialog",
            520.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        ))
    }

    /// The name of one place for a person: what kind of record, which one, and
    /// which field - "SSH host · web · address".
    fn document_place_title(&self, place: &shellune_storage::DocumentConflict) -> String {
        use shellune_storage::ConflictSection;
        let strings = self.sync_strings();
        let section = match place.section {
            ConflictSection::Host => strings.section_host,
            ConflictSection::Connection => strings.section_connection,
            ConflictSection::Favorites => strings.section_favorites,
            ConflictSection::Groups => strings.section_groups,
            ConflictSection::Forward => strings.section_forward,
            ConflictSection::Snippet => strings.section_snippet,
            ConflictSection::KnownHost => strings.section_known_host,
            ConflictSection::Preferences => strings.section_preferences,
            ConflictSection::ThisPlatform => strings.section_platform,
            ConflictSection::Other => strings.section_other,
        };
        // A machine-specific field of this platform names its host by id; the
        // book knows its name.
        let record = place.record.as_ref().map(|record| {
            self.book
                .records()
                .iter()
                .find(|known| known.id().to_string() == *record)
                .map_or_else(|| record.clone(), |known| known.name().to_owned())
        });
        let field = if place.field.is_empty() {
            strings.records_whole.to_owned()
        } else {
            place
                .field
                .split('.')
                .map(|part| field_label(part, strings))
                .collect::<Vec<_>>()
                .join(" \u{b7} ")
        };
        let mut parts = vec![section.to_owned()];
        parts.extend(record);
        parts.push(field);
        parts.join(" \u{b7} ")
    }

    /// One place both sides changed differently, with both values and the two
    /// ways to answer it.
    fn document_place_row(
        &self,
        index: usize,
        place: &shellune_storage::DocumentConflict,
        chosen: Option<shellune_storage::MergeSide>,
        cx: &mut Context<Self>,
    ) -> Div {
        use shellune_storage::MergeSide;
        let strings = self.sync_strings();
        let metrics = self.metrics();
        let value = |template: &'static str, value: &Option<String>| {
            copy::fill(
                template,
                &[("value", value.as_deref().unwrap_or(strings.records_absent))],
            )
        };
        let line = |text: String, picked: bool, selector: String| {
            div()
                .min_w_0()
                .truncate()
                .font_family(crate::app::terminal::monospace_family())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(if picked {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_SECONDARY
                }))
                .when(picked, |line| line.font_weight(FontWeight::MEDIUM))
                .debug_selector(move || selector.clone())
                .child(SharedString::from(text))
        };
        let keep = |side: MergeSide, selector: String, label: &'static str| {
            let picked = chosen == Some(side);
            components::compact_button(
                label,
                if picked {
                    Variant::Primary
                } else {
                    Variant::Secondary
                },
                metrics,
            )
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.choose_document_side(index, side, cx);
            }))
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .p(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .border_color(theme::swatch(if chosen.is_some() {
                palette::BORDER
            } else {
                palette::BORDER_STRONG
            }))
            .debug_selector(move || format!("sync.records.row.{index}"))
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(move || format!("sync.records.title.{index}"))
                    .child(SharedString::from(self.document_place_title(place))),
            )
            .child(line(
                value(strings.records_local_value, &place.local),
                chosen == Some(MergeSide::Local),
                format!("sync.records.local.value.{index}"),
            ))
            .child(line(
                value(strings.records_remote_value, &place.remote),
                chosen == Some(MergeSide::Remote),
                format!("sync.records.remote.value.{index}"),
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(keep(
                        MergeSide::Local,
                        format!("sync.records.local.{index}"),
                        strings.vault_keep_local,
                    ))
                    .child(keep(
                        MergeSide::Remote,
                        format!("sync.records.remote.{index}"),
                        strings.vault_keep_remote,
                    )),
            )
    }

    /// The places that differ, read-only, for a conflict only a whole side can
    /// settle: the first few, and how many more.
    fn document_places_preview(
        &self,
        report: &ConflictReport,
        metrics: crate::app::theme::Metrics,
    ) -> Div {
        const SHOWN: usize = 6;
        let strings = self.sync_strings();
        let mut list = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "sync.conflict.places".to_owned())
            .child(
                div()
                    .whitespace_normal()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(strings.records_where_hint),
            );
        for (index, place) in report.document.iter().take(SHOWN).enumerate() {
            let local = place.local.as_deref().unwrap_or(strings.records_absent);
            let remote = place.remote.as_deref().unwrap_or(strings.records_absent);
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .debug_selector(move || format!("sync.conflict.place.{index}"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics.caption_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(self.document_place_title(place))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(format!("{local}  \u{2192}  {remote}"))),
                    ),
            );
        }
        if report.document.len() > SHOWN {
            list = list.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(copy::fill(
                        strings.records_more,
                        &[("count", &(report.document.len() - SHOWN).to_string())],
                    ))),
            );
        }
        list
    }

    /// The vault half of a conflict: one row per record or trusted identity,
    /// each kept from this device or from the cloud on its own (ADR-5, R8).
    /// The apply button stays inert until the last open one is answered.
    fn vault_conflict_overlay(
        &self,
        report: &ConflictReport,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        use shellune_sync::{VaultChoiceSide, VaultConflict, VaultRecordSide};
        let strings = self.sync_strings();
        let metrics = self.metrics();
        let only_document = report.vault.is_empty();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Refresh),
            if only_document {
                strings.conflict_title
            } else {
                strings.vault_conflict_title
            }
            .to_owned(),
            None,
            "sync.vault.conflict.close",
            cx,
            |this, cx| this.resolve_sync_conflict(Resolution::Cancel, cx),
        );
        let source = copy::fill(
            strings.conflict_source,
            &[
                ("platform", &platform_label(&report.remote_platform)),
                ("version", &report.remote_app_version),
                (
                    "date",
                    &crate::app::hosts::format_unix(report.remote_created_unix),
                ),
            ],
        );
        let mut body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "sync.vault.conflict.body".to_owned())
            .child(
                components::banner(Tone::Warning, metrics)
                    .whitespace_normal()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "sync.vault.conflict.intro".to_owned())
                    .child(if only_document {
                        strings.records_conflict_intro
                    } else {
                        strings.vault_conflict_intro
                    }),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "sync.vault.conflict.source".to_owned())
                    .child(SharedString::from(source)),
            );
        let mut all_answered = true;
        for (index, conflict) in report.vault.iter().enumerate() {
            let chosen = self
                .sync
                .vault_choices
                .get(index)
                .and_then(|(_, side)| *side);
            all_answered &= chosen.is_some();
            let (label, local_line, remote_line) = match conflict {
                VaultConflict::Record {
                    name,
                    local,
                    remote,
                    ..
                } => {
                    let side = |side: &VaultRecordSide, deleted: &'static str, device: String| {
                        if side.deleted {
                            deleted.to_owned()
                        } else {
                            copy::fill(
                                strings.vault_side_device,
                                &[("device", &device), ("count", &side.counter.to_string())],
                            )
                        }
                    };
                    let label = if local.deleted || remote.deleted {
                        copy::fill(strings.vault_record_deleted_label, &[("name", name)])
                    } else {
                        copy::fill(
                            strings.vault_record_label,
                            &[
                                ("name", name),
                                ("count", &local.counter.max(remote.counter).to_string()),
                            ],
                        )
                    };
                    (
                        label,
                        side(local, strings.vault_side_deleted, local.device_id.clone()),
                        side(remote, strings.vault_side_deleted, remote.device_id.clone()),
                    )
                }
                VaultConflict::Trust {
                    host,
                    port,
                    local,
                    remote,
                    ..
                } => {
                    let identity = |identity: &Option<shellune_sync::TrustIdentity>| {
                        identity
                            .as_ref()
                            .map(|identity| {
                                copy::fill(
                                    strings.vault_side_fingerprint,
                                    &[
                                        ("algorithm", &identity.algorithm),
                                        ("fingerprint", &identity.fingerprint),
                                    ],
                                )
                            })
                            .unwrap_or_else(|| strings.vault_side_deleted.to_owned())
                    };
                    (
                        copy::fill(
                            strings.vault_trust_label,
                            &[("host", host), ("port", &port.to_string())],
                        ),
                        identity(local),
                        identity(remote),
                    )
                }
            };
            let mut row = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .gap(metrics.scale(spacing::HAIRLINE))
                .p(metrics.scale(spacing::TIGHT))
                .rounded(px(radius::MEDIUM))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .debug_selector(move || format!("sync.vault.conflict.row.{index}"))
                .child(
                    div()
                        .text_size(metrics.label_size())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(crate::app::terminal::monospace_family())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(local_line)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(crate::app::terminal::monospace_family())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(remote_line)),
                );
            let keep = |which: u8, picked: bool, selector: String, label: &'static str| {
                components::compact_button(label, Variant::Secondary, metrics)
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .when(picked, |button| button.opacity(0.5))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        let side = if which == 0 {
                            VaultChoiceSide::Local
                        } else {
                            VaultChoiceSide::Remote
                        };
                        this.choose_vault_side(index, side, cx);
                    }))
            };
            row = row.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(keep(
                        0,
                        chosen == Some(VaultChoiceSide::Local),
                        format!("sync.vault.conflict.local.{index}"),
                        strings.vault_keep_local,
                    ))
                    .child(keep(
                        1,
                        chosen == Some(VaultChoiceSide::Remote),
                        format!("sync.vault.conflict.remote.{index}"),
                        strings.vault_keep_remote,
                    )),
            );
            body = body.child(row);
        }
        // The data's own places, answered beside the vault's.
        let answerable = report.per_record && !report.document.is_empty();
        if answerable {
            body = body.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::TIGHT))
                    .w_full()
                    .child(
                        div()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(strings.records_where),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(metrics.scale(spacing::HAIRLINE))
                            .child(
                                components::compact_button(
                                    strings.records_all_local,
                                    Variant::Ghost,
                                    metrics,
                                )
                                .id("sync.records.all.local")
                                .debug_selector(|| "sync.records.all.local".to_owned())
                                .on_click(cx.listener(
                                    |this, _event, _window, cx| {
                                        this.choose_every_side(true, cx);
                                    },
                                )),
                            )
                            .child(
                                components::compact_button(
                                    strings.records_all_remote,
                                    Variant::Ghost,
                                    metrics,
                                )
                                .id("sync.records.all.remote")
                                .debug_selector(|| "sync.records.all.remote".to_owned())
                                .on_click(cx.listener(
                                    |this, _event, _window, cx| {
                                        this.choose_every_side(false, cx);
                                    },
                                )),
                            ),
                    ),
            );
            for (index, place) in report.document.iter().enumerate() {
                let chosen = self
                    .sync
                    .document_choices
                    .get(index)
                    .and_then(|(_, side)| *side);
                all_answered &= chosen.is_some();
                body = body.child(self.document_place_row(index, place, chosen, cx));
            }
        }
        body = body
            .when(!answerable, |body| {
                body.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "sync.vault.conflict.then".to_owned())
                        .child(strings.vault_then_document),
                )
            })
            .capture_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.resolve_sync_conflict(Resolution::Cancel, cx);
                    }
                }),
            );
        let apply = components::compact_button(
            strings.vault_apply,
            if all_answered {
                Variant::Primary
            } else {
                Variant::Secondary
            },
            metrics,
        )
        .id("sync.vault.conflict.apply")
        .debug_selector(|| "sync.vault.conflict.apply".to_owned())
        .when(!all_answered, |button| button.opacity(0.5))
        .when(all_answered, |button| {
            button.on_click(cx.listener(|this, _event, _window, cx| {
                this.resolve_sync_vault(cx);
            }))
        });
        // A conflict of the data alone may still be settled by keeping a whole
        // side; a vault conflict is answered record by record only (ADR-5).
        let (dialog_selector, cancel_selector) = if only_document {
            ("sync.conflict.dialog", "sync.conflict.cancel")
        } else {
            ("sync.vault.conflict.dialog", "sync.vault.conflict.cancel")
        };
        let footer = components::modal_footer(metrics)
            .child(
                self.text_button(
                    SharedString::from(cancel_selector),
                    strings.cancel,
                    Variant::Ghost,
                    cx,
                    |this, cx| this.resolve_sync_conflict(Resolution::Cancel, cx),
                )
                .h(metrics.control_height()),
            )
            .when(only_document, |footer| {
                footer
                    .child(self.text_button(
                        SharedString::from("sync.conflict.local"),
                        strings.use_local,
                        Variant::Ghost,
                        cx,
                        |this, cx| this.resolve_sync_conflict(Resolution::UseLocal, cx),
                    ))
                    .child(self.text_button(
                        SharedString::from("sync.conflict.remote"),
                        strings.use_remote,
                        Variant::Ghost,
                        cx,
                        |this, cx| this.resolve_sync_conflict(Resolution::UseRemote, cx),
                    ))
            })
            .child(apply);
        self.modal(
            window,
            dialog_selector,
            560.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// What the tests and the captures read
// ---------------------------------------------------------------------------

impl ShelluneView {
    /// How many sync jobs ended, whether one runs, and the reason of the last failure.
    #[cfg(test)]
    #[must_use]
    pub fn sync_progress_for_test(&self) -> (usize, bool, Option<String>) {
        (
            self.sync.finished,
            self.sync_slot_busy()
                || self.sync.queued.is_some()
                || self.sync.waiting_backup.is_some(),
            self.sync.failure.clone(),
        )
    }

    /// Marks the last sync as failed, against a server that is saved but never asked,
    /// so a test can see what the top row does with the marker.
    #[cfg(test)]
    pub fn fail_sync_for_test(&mut self, cx: &mut Context<Self>) {
        self.sync.network = false;
        self.sync.config = Some(SyncConfig {
            url: "https://dav.example.invalid/shellune".to_owned(),
            ..SyncConfig::default()
        });
        self.sync.failure = Some("offline".to_owned());
        cx.notify();
    }

    /// Whether the conflict question is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn sync_conflict_open_for_test(&self) -> bool {
        self.sync_dialog_open()
    }

    /// Stops the next finished job before the window takes it in: what it wrote is on
    /// disk, and the window has not read it back yet.
    #[cfg(test)]
    pub fn hold_sync_finish_for_test(&mut self) {
        self.sync.hold_finish = true;
    }

    /// Whether a finished job is being held.
    #[cfg(test)]
    #[must_use]
    pub fn sync_finish_held_for_test(&self) -> bool {
        self.sync.held_finish.is_some()
    }

    /// Lets the held job be taken in.
    #[cfg(test)]
    pub fn release_sync_finish_for_test(&mut self, cx: &mut Context<Self>) {
        self.sync.hold_finish = false;
        if let Some((job, result)) = self.sync.held_finish.take() {
            self.finish_sync_job(job, result, cx);
        }
    }

    /// Whether the window's saves are held back.
    #[cfg(test)]
    #[must_use]
    pub fn data_writes_held_for_test(&self) -> bool {
        self.data_writes_held()
    }

    /// Whether a close or quit waits for held saves to be written.
    #[cfg(test)]
    #[must_use]
    pub fn exit_waiting_for_test(&self) -> bool {
        self.exit_after_held_writes.is_some()
    }

    /// Checks the cloud as "resolve conflict" would without one: a sync in either
    /// direction, started by a person.
    #[cfg(test)]
    pub fn check_sync_for_test(&mut self, cx: &mut Context<Self>) {
        self.request_sync(SyncJob::Check, cx);
    }

    /// Opens the conflict question with a fixed report; nothing is resolved.
    #[cfg(test)]
    pub fn open_sync_conflict_for_test(&mut self, cx: &mut Context<Self>) {
        self.sync.conflict = Some(ConflictReport {
            first_sync: false,
            remote_platform: "linux".to_owned(),
            remote_app_version: env!("CARGO_PKG_VERSION").to_owned(),
            remote_created_unix: 1_794_816_000,
            vault: Vec::new(),
            document: Vec::new(),
            per_record: false,
        });
        self.sync.dialog = true;
        cx.notify();
    }

    /// Opens the vault half of a conflict with one record and one trust
    /// disagreement, the shape ADR-5 asks the user about one by one. Nothing
    /// is applied by opening it.
    #[cfg(test)]
    pub fn open_sync_vault_conflict_for_test(&mut self, cx: &mut Context<Self>) {
        use shellune_sync::{TrustIdentity, VaultConflict, VaultRecordKind, VaultRecordSide};
        let report = ConflictReport {
            first_sync: false,
            remote_platform: "linux".to_owned(),
            remote_app_version: env!("CARGO_PKG_VERSION").to_owned(),
            remote_created_unix: 1_794_816_000,
            vault: vec![
                VaultConflict::Record {
                    kind: VaultRecordKind::Credential,
                    id: "credential-1".to_owned(),
                    name: "Prod login".to_owned(),
                    local: VaultRecordSide {
                        name: "Prod login".to_owned(),
                        deleted: false,
                        counter: 3,
                        device_id: "device-a".to_owned(),
                    },
                    remote: VaultRecordSide {
                        name: "Prod login".to_owned(),
                        deleted: false,
                        counter: 4,
                        device_id: "device-b".to_owned(),
                    },
                },
                VaultConflict::Trust {
                    host: "web.example.com".to_owned(),
                    port: 22,
                    local: Some(TrustIdentity {
                        algorithm: "ssh-ed25519".to_owned(),
                        fingerprint: "SHA256:locallocallocallocallocallocallocal".to_owned(),
                        accepted_unix: 1,
                    }),
                    remote: Some(TrustIdentity {
                        algorithm: "ssh-ed25519".to_owned(),
                        fingerprint: "SHA256:remoteremoteremoteremoteremoterem".to_owned(),
                        accepted_unix: 2,
                    }),
                    remote_source_platform: "linux".to_owned(),
                    remote_app_version: env!("CARGO_PKG_VERSION").to_owned(),
                    remote_created_unix: 1_794_816_000,
                },
            ],
            document: Vec::new(),
            per_record: false,
        };
        self.sync.vault_choices = report
            .vault
            .iter()
            .map(|conflict| (choice_key_of(conflict), None))
            .collect();
        self.sync.conflict = Some(report);
        self.sync.dialog = true;
        cx.notify();
    }

    /// Opens a conflict whose data part names two places both sides changed,
    /// answerable one by one, as a sync with a base reports it.
    #[cfg(test)]
    pub fn open_sync_record_conflict_for_test(&mut self, cx: &mut Context<Self>) {
        use shellune_storage::{ConflictSection, DocumentConflict};
        let report = ConflictReport {
            first_sync: false,
            remote_platform: "windows".to_owned(),
            remote_app_version: env!("CARGO_PKG_VERSION").to_owned(),
            remote_created_unix: 1_794_816_000,
            vault: Vec::new(),
            document: vec![
                DocumentConflict {
                    key: "shared/hosts/hosts/[a]/address".to_owned(),
                    section: ConflictSection::Host,
                    record: Some("web".to_owned()),
                    field: "address".to_owned(),
                    local: Some("10.0.0.3".to_owned()),
                    remote: Some("10.0.0.2".to_owned()),
                },
                DocumentConflict {
                    key: "shared/hosts/hosts/[b]".to_owned(),
                    section: ConflictSection::Host,
                    record: Some("db".to_owned()),
                    field: String::new(),
                    local: None,
                    remote: Some("{…}".to_owned()),
                },
            ],
            per_record: true,
        };
        self.sync.vault_choices = Vec::new();
        self.sync.document_choices = document_choices_of(&report);
        self.sync.conflict = Some(report);
        self.sync.dialog = true;
        cx.notify();
    }

    /// The answers chosen so far for the data part of the open conflict.
    #[cfg(test)]
    pub fn sync_document_choices_for_test(
        &self,
    ) -> Vec<(String, Option<shellune_storage::MergeSide>)> {
        self.sync.document_choices.clone()
    }

    /// Whether an automatic sync is waiting for the vault to be unlocked.
    #[cfg(test)]
    #[must_use]
    pub fn sync_deferred_for_test(&self) -> bool {
        self.sync.deferred_locked
    }

    /// What the connection test said: `None` before or while it runs, then whether it
    /// passed and its sentence.
    #[cfg(test)]
    #[must_use]
    pub fn sync_test_for_test(&self) -> Option<(bool, String)> {
        match &self.sync.test {
            TestStatus::Idle | TestStatus::Running => None,
            TestStatus::Passed => Some((true, self.sync_strings().test_ok.to_owned())),
            TestStatus::Failed(reason) => Some((false, reason.clone())),
        }
    }

    /// The status sentence and the form's refusal.
    #[cfg(test)]
    #[must_use]
    pub fn sync_status_for_test(&self) -> (String, Option<String>) {
        (self.sync_status_text().0, self.sync.form_problem.clone())
    }

    /// Whether the top bar draws the sync marker, and which one.
    #[cfg(test)]
    #[must_use]
    pub fn sync_marker_for_test(&self) -> Option<&'static str> {
        if !self.sync.configured() || self.sync.running.is_some() {
            return None;
        }
        if self.sync.has_conflict() {
            Some("conflict")
        } else if self.sync.has_failure() {
            Some("failed")
        } else {
            None
        }
    }

    /// Opens the sync category, for a capture. The capture window is offline: the page
    /// is drawn from the saved settings and state, and no job ever starts.
    #[cfg(feature = "ui-capture")]
    pub fn capture_sync_settings(&mut self, cx: &mut Context<Self>) {
        self.sync.network = false;
        self.settings_reveal(Category::Sync);
        self.settings_select_category(Category::Sync, cx);
    }

    /// Opens the conflict question with a fixed report, for a capture: nothing is
    /// resolved.
    #[cfg(feature = "ui-capture")]
    pub fn capture_sync_conflict(&mut self, cx: &mut Context<Self>) {
        self.capture_sync_settings(cx);
        self.sync.conflict = Some(ConflictReport {
            first_sync: false,
            remote_platform: "macos".to_owned(),
            remote_app_version: env!("CARGO_PKG_VERSION").to_owned(),
            remote_created_unix: 1_794_816_000,
            vault: Vec::new(),
            document: Vec::new(),
            per_record: false,
        });
        self.sync.dialog = true;
        cx.notify();
    }

    /// Marks the last sync as failed against a saved server that is never asked, for a
    /// capture of the top row's marker; the window stays offline.
    #[cfg(feature = "ui-capture")]
    pub fn capture_sync_failed(&mut self, cx: &mut Context<Self>) {
        self.sync.network = false;
        self.sync.config = Some(SyncConfig {
            url: "https://dav.example.invalid/shellune".to_owned(),
            ..SyncConfig::default()
        });
        self.sync.failure = Some(self.sync_strings().marker_failed.to_owned());
        cx.notify();
    }

    /// Whether the top row has a sync marker to show, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_sync_marked(&self) -> bool {
        self.sync_notice().is_some()
    }

    /// Whether the conflict question is on screen, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_sync_conflict_open(&self) -> bool {
        self.sync_dialog_open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_failure_is_said_in_the_window_s_language() {
        let zh = copy::sync_for(copy::Locale::Chinese);
        let unreachable = SyncError::Remote(WebDavError::Unreachable {
            reason: "connection timed out".to_owned(),
        })
        .to_string();
        let said = saved_reason_text(&unreachable, zh);
        assert!(said.starts_with(zh.error_unreachable), "{said}");
        assert!(said.contains("connection timed out"), "{said}");

        let auth = SyncError::Remote(WebDavError::Auth { status: 401 }).to_string();
        assert_eq!(saved_reason_text(&auth, zh), zh.error_auth);

        let folder = SyncError::Remote(WebDavError::NotWritable {
            path: "/shellune".to_owned(),
            status: 403,
        })
        .to_string();
        assert_eq!(
            saved_reason_text(&folder, zh),
            copy::fill(zh.error_not_writable, &[("path", "/shellune")])
        );

        let status = SyncError::Remote(WebDavError::Status {
            method: "PUT".to_owned(),
            status: 507,
            location: None,
        })
        .to_string();
        assert!(saved_reason_text(&status, zh).starts_with(zh.error_server));

        for (error, expected) in [
            (SyncError::NothingInCloud, zh.error_nothing_in_cloud),
            (SyncError::PassphraseMissing, zh.error_passphrase_missing),
            (SyncError::Busy, zh.error_busy),
            (
                SyncError::RemoteDocument(PortableError::WrongPassphrase),
                zh.error_remote_passphrase,
            ),
        ] {
            assert_eq!(saved_reason_text(&error.to_string(), zh), expected);
        }
        let damaged = SyncError::RemoteDocument(PortableError::Corrupt {
            reason: "bad json".to_owned(),
        })
        .to_string();
        let said = saved_reason_text(&damaged, zh);
        assert!(said.starts_with(zh.error_remote_document), "{said}");

        let credentials = format!("{CREDENTIALS_PREFIX}the store is locked");
        assert!(saved_reason_text(&credentials, zh).starts_with(zh.error_credentials));

        let unknown = saved_reason_text("something new went wrong", zh);
        assert!(unknown.starts_with(zh.error_last_failed), "{unknown}");
        assert!(unknown.contains("something new went wrong"));
    }

    #[test]
    fn a_saved_rollback_is_recognised_and_said_with_both_versions() {
        let saved = shellune_sync::SyncError::Rollback {
            found: 4,
            applied: 5,
        }
        .to_string();
        assert_eq!(super::rollback_versions(&saved), Some((4, 5)));
        assert_eq!(super::rollback_versions("the data kept changing"), None);
        let text = super::saved_reason_text(&saved, copy::sync_for(copy::Locale::Chinese));
        assert!(
            text.contains('4') && text.contains('5') && !text.contains("cloud holds"),
            "{text}"
        );
    }

    #[test]
    fn the_credentials_sentence_matches_the_engine() {
        // The one sentence spelled out here must stay the engine's own wording.
        let source = include_str!("../../../crates/sync/src/engine.rs");
        assert!(
            source.contains(&format!("#[error(\"{CREDENTIALS_PREFIX}{{0}}\")]")),
            "SyncError::Credentials still reads \"{CREDENTIALS_PREFIX}{{0}}\""
        );
    }
}
