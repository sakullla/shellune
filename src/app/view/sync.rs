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
//! * a failure never blocks the window or loses a local change: the change is already on
//!   disk, the reason is shown in the settings and as a marker in the top bar, and the
//!   next change or check tries again;
//! * a conflict is a question with three answers, and nothing is overwritten before the
//!   user answers it. Dismissing it keeps the conflict (and the marker) until it is
//!   answered; automatic checks do not ask again until it is, a manual one does.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Div, Entity, FontWeight, SharedString, Stateful, Window, div, px};
use shellune_core::SecretString;
use shellune_storage::{
    KdfParams, LocalData, Platform as DataPlatform, PortableError, SecretStore,
};
use shellune_sync::{
    ConflictReport, LastResult, PullReport, Resolution, SyncConfig, SyncError, SyncOutcome,
    SyncService, SyncState, WebDavError,
};

use super::ShelluneView;
use super::settings::Category;
use super::shell::ModalContent;
use crate::app::components::{self, Variant};
use crate::app::copy::{self, SyncStrings};
use crate::app::icons::Icon;
use crate::app::nav::Destination;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Tone, palette, spacing};

/// How long after the last saved change an automatic upload starts. Edits made in a
/// burst become one upload.
pub(super) const DEBOUNCE: Duration = Duration::from_secs(5);

/// How often the cloud is checked while automatic sync is on.
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// One sync the window asks the engine for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl SyncJob {
    /// Whether a person asked for it, so its outcome is worth a notification.
    fn manual(self) -> bool {
        !matches!(self, Self::Auto)
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
    /// The password switch as shown (saved with the form, since it needs a passphrase).
    draft_secrets: bool,
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
    /// Whether the conflict question is on screen.
    dialog: bool,
    /// The user put the question away; automatic checks do not ask again.
    conflict_dismissed: bool,
    /// How many jobs ended, for the tests.
    finished: usize,
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
            draft_secrets: shown.sync_secrets,
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
            dialog: false,
            conflict_dismissed: false,
            finished: 0,
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
        if self.sync.running.is_some() {
            self.sync.queued = match (self.sync.queued, job) {
                (Some(waiting), SyncJob::Auto) => Some(waiting),
                (_, job) => Some(job),
            };
            return;
        }
        self.sync.running = Some(job);
        let service = self.sync.service.clone();
        let work = cx.background_executor().spawn(async move {
            match job {
                SyncJob::Auto | SyncJob::Check => service.sync(),
                SyncJob::Upload => service.upload(),
                SyncJob::Pull => service.pull(),
                SyncJob::Resolve(resolution) => service.resolve(resolution),
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
        self.sync.running = None;
        self.sync.finished += 1;
        self.sync.state = self.sync.service.state();
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
                self.adopt_pulled_data(&report, cx);
            }
            Ok(SyncOutcome::Conflict(report)) => {
                self.sync.failure = None;
                let ask = job.manual() || !self.sync.conflict_dismissed;
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
            }
        }
        if let Some(next) = self.sync.queued.take() {
            self.run_sync_job(next, cx);
        }
        cx.notify();
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
    fn adopt_pulled_data(&mut self, report: &PullReport, cx: &mut Context<Self>) {
        let strings = self.sync_strings();
        self.sync.applying = true;
        self.adopt_imported_data(report.preferences_changed || report.keymap_changed, cx);
        self.key_presence.forget();
        self.sync.applying = false;
        let mut notes = Vec::new();
        if !report.trust_conflicts.is_empty() {
            notes.push(copy::fill(
                strings.pull_trust,
                &[("count", &report.trust_conflicts.len().to_string())],
            ));
        }
        if report.secrets_ignored {
            notes.push(strings.pull_secrets_ignored.to_owned());
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
            strings.toast_pulled,
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
            sync_secrets: self.sync.draft_secrets,
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
        if draft.sync_secrets && !self.sync.passphrase_saved && passphrase.trim().is_empty() {
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
            if draft.sync_secrets && !passphrase.trim().is_empty() {
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
                if draft.sync_secrets && !passphrase.trim().is_empty() {
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

    /// Turns syncing passwords on or off in the form; Save applies it, since turning it
    /// on needs the passphrase.
    pub(super) fn set_sync_secrets(&mut self, on: bool, cx: &mut Context<Self>) {
        self.sync.draft_secrets = on;
        self.sync.form_problem = None;
        if on {
            self.field_focus_pending = Some(self.sync.passphrase.read(cx).focus_handle().clone());
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
                .w(metrics.scale(240.0))
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
            .min_w(px(0.0))
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
                        self.sync.failure.as_deref().unwrap_or(reason.as_str()),
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
            view.segments(
                id,
                vec![(strings.on.to_owned(), on), (strings.off.to_owned(), !on)],
                cx,
                move |view, index, cx| pick(view, index == 0, cx),
            )
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
            self.settings_row(
                "sync-secrets",
                strings.secrets,
                strings.secrets_hint,
                switch(
                    self,
                    "settings.sync.secrets",
                    self.sync.draft_secrets,
                    cx,
                    Self::set_sync_secrets,
                ),
            ),
        ];
        if self.sync.draft_secrets {
            rows.push(self.sync_field_row(
                "sync-passphrase",
                strings.passphrase,
                strings.passphrase_hint,
                &self.sync.passphrase,
            ));
        }
        let testing = self.sync.test == TestStatus::Running;
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w(px(0.0))
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
        if self.sync.configured() && self.sync.has_failure() && self.sync.running.is_none() {
            rows.push(self.sync_line(strings.retry_note, None, "settings.sync.retry"));
        }
        self.settings_card("sync", rows)
    }

    /// The marker in the top bar while the last sync failed or a conflict waits; a press
    /// opens the sync settings.
    pub(super) fn sync_topbar_marker(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.sync.configured() || self.sync.running.is_some() {
            return None;
        }
        let strings = self.sync_strings();
        let (label, tone) = if self.sync.has_conflict() {
            (strings.marker_conflict, Tone::Warning)
        } else if self.sync.has_failure() {
            (strings.marker_failed, Tone::Danger)
        } else {
            return None;
        };
        let metrics = self.metrics();
        Some(
            div()
                .id("topbar.sync")
                .debug_selector(|| "topbar.sync".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .mr(metrics.scale(spacing::HAIRLINE))
                .cursor_pointer()
                .tooltip(components::tooltip(SharedString::from(strings.marker_hint)))
                .child(components::chip(label, tone, metrics))
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.open_sync_settings(window, cx);
                })),
        )
    }

    /// The conflict question over the window (R10).
    pub(super) fn sync_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.sync_dialog_open() {
            return None;
        }
        let report = self.sync.conflict.as_ref()?;
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
                .min_w(px(0.0))
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
            .min_w(px(0.0))
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
                    .min_w(px(0.0))
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "sync.conflict.source".to_owned())
                    .child(SharedString::from(source)),
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
        let footer = div()
            .flex()
            .items_center()
            .justify_end()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::SNUG))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
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
            self.sync.running.is_some() || self.sync.queued.is_some(),
            self.sync.failure.clone(),
        )
    }

    /// Whether the conflict question is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn sync_conflict_open_for_test(&self) -> bool {
        self.sync_dialog_open()
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
        });
        self.sync.dialog = true;
        cx.notify();
    }

    /// Whether the conflict question is on screen, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_sync_conflict_open(&self) -> bool {
        self.sync_dialog_open()
    }
}
