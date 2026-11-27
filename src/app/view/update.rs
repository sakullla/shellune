//! Updates in the window: the automatic check, the about-and-updates page, the chip in
//! the top bar and the question asked before an install (R2, R3).
//!
//! The update calls block, so each one runs on the background executor and the window
//! only takes in what it returned:
//!
//! * a while after the window opens ([`STARTUP_CHECK_DELAY`]) one check runs, when the
//!   automatic switch is on. A failure of that check is not shown: nobody asked, and
//!   the next start tries again. A check a person asked for says why it failed;
//! * a newer release puts a chip in the top bar on every page; a press opens the update
//!   card, which says what the release is, what is new, and what can be done: download
//!   and install, or - when this copy cannot install it itself - why not, and its page;
//! * an install or a restart stops the sessions of this window. With a terminal, a
//!   transfer, a remote desktop or a serial session running, the window asks first and
//!   says how many of each would stop; "later" leaves the update downloaded and waiting.
//!   Once confirmed the tabs are saved, and while a sync or an import holds the window's
//!   saves back the update waits for them to be written - and is called off when some
//!   could not be kept - before anything is started or the window exits;
//! * the Windows installer replaces the files after the window exits, and starts the new
//!   version itself. The portable executable and the macOS bundle are replaced in place,
//!   and the card offers the restart. A copy that is updated from its release page is
//!   sent there.
//!
//! Every failure keeps the running copy as it was, and the card says which step failed
//! and why, in words that tell a missing signature, a release without a file for this
//! computer, a signature that does not verify and a download that stopped apart.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, SharedString, Stateful, Window, div, px};
use shellune_core::SessionKind;
use shellune_update::{
    CheckOutcome, DownloadProgress, DownloadedUpdate, InstallOutcome, InstallRoute, Release,
    Unavailable, UpdateError,
};

use super::ShelluneView;
use super::settings::Category;
use super::shell::{ModalContent, NoticeForm, notice_icon, notice_words};
use super::sync::PendingExit;
use crate::app::components::{self, Variant};
use crate::app::copy::{self, UpdateStrings};
use crate::app::icons::Icon;
use crate::app::nav::Destination;
use crate::app::theme::{self, Tone, palette, radius, spacing};
use crate::app::update::{
    ActiveWork, ConfirmPurpose, PROGRESS_INTERVAL, STARTUP_CHECK_DELAY, UpdateBackend, UpdateStage,
    UpdateStep, error_text, notes_excerpt, progress_fraction, progress_text, unavailable_text,
};

/// The window width, before the text scale, from which the top bar's chip may say "new
/// version" in words; below it the chip is an arrow with the words in its tooltip. The
/// top row's own rule for its notices ([`ShelluneView::topbar_notice_form`]) can ask
/// for more.
pub(super) const TOPBAR_FULL_CHIP_WIDTH: f32 = 900.0;

/// How often an update that waits for held saves looks again.
const HELD_WRITES_POLL: Duration = Duration::from_millis(200);

/// The update state of one window.
pub(super) struct UpdateCenter {
    /// The update calls; `None` when this window never updates (a capture, a test that
    /// did not ask for it, or a build whose update settings are unusable).
    backend: Option<Arc<dyn UpdateBackend>>,
    /// Why there is no backend, when its settings were refused.
    unusable: Option<UpdateError>,
    /// How this copy is updated.
    route: Option<InstallRoute>,
    stage: UpdateStage,
    /// The automatic switch (a preference of this computer, absent means on).
    auto_check: bool,
    /// What the running download has received, written by the download itself.
    progress: Arc<Mutex<Option<DownloadProgress>>>,
    /// The question on screen before an install or a restart.
    confirm: Option<ConfirmPurpose>,
    /// An install or a restart that waits for held saves to be written.
    waiting: Option<ConfirmPurpose>,
    /// The count of held saves that could not be kept, from before the wait.
    lost_before: u64,
    /// How many checks ended, for the tests.
    checks: usize,
    /// The window asked to exit so the installer or the new version can take over.
    exiting: bool,
}

impl UpdateCenter {
    /// The update state over `backend`; nothing is sent until the window starts it.
    pub(super) fn new(backend: Result<Option<Arc<dyn UpdateBackend>>, UpdateError>) -> Self {
        let (backend, unusable) = match backend {
            Ok(backend) => (backend, None),
            Err(error) => (None, Some(error)),
        };
        let route = backend.as_ref().map(|backend| backend.route());
        Self {
            backend,
            unusable,
            route,
            stage: UpdateStage::Idle,
            auto_check: true,
            progress: Arc::new(Mutex::new(None)),
            confirm: None,
            waiting: None,
            lost_before: 0,
            checks: 0,
            exiting: false,
        }
    }

    /// Whether the automatic check is on.
    pub(super) fn auto_check(&self) -> bool {
        self.auto_check
    }

    /// Sets the automatic check as a saved preference says.
    pub(super) fn set_auto_check(&mut self, on: bool) {
        self.auto_check = on;
    }
}

impl ShelluneView {
    /// The update strings of the language on screen.
    fn update_strings(&self) -> &'static UpdateStrings {
        copy::update_for(self.workspace.locale())
    }

    /// Starts the window's update work: removes what earlier updates left, and checks
    /// once after [`STARTUP_CHECK_DELAY`] when the automatic switch is on then. Without
    /// a backend nothing happens.
    pub(super) fn start_updates(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.update.backend.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move { backend.clean_up() })
            .detach();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STARTUP_CHECK_DELAY).await;
            let _ = this.update(cx, |view, cx| {
                if view.update.auto_check && matches!(view.update.stage, UpdateStage::Idle) {
                    view.check_for_updates(false, cx);
                }
            });
        })
        .detach();
    }

    /// Reads the manifest on the background executor. `manual` when a person asked:
    /// only then is a failure shown, and "already the newest" said.
    pub(super) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update.stage.busy()
            || matches!(
                self.update.stage,
                UpdateStage::Ready { .. } | UpdateStage::Restart { .. }
            )
        {
            return;
        }
        let Some(backend) = self.update.backend.clone() else {
            if manual {
                self.update.stage = UpdateStage::Failed {
                    step: UpdateStep::Check,
                    error: self
                        .update
                        .unusable
                        .clone()
                        .unwrap_or(UpdateError::InvalidConfig {
                            reason: "this window does not update".to_owned(),
                        }),
                    release: None,
                    update: None,
                };
                cx.notify();
            }
            return;
        };
        let before = std::mem::replace(&mut self.update.stage, UpdateStage::Checking { manual });
        let work = cx
            .background_executor()
            .spawn(async move { backend.check() });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_update_check(manual, before, result, cx)
            });
        })
        .detach();
        cx.notify();
    }

    /// Takes in what a check found.
    fn finish_update_check(
        &mut self,
        manual: bool,
        before: UpdateStage,
        result: Result<CheckOutcome, UpdateError>,
        cx: &mut Context<Self>,
    ) {
        self.update.checks += 1;
        let strings = self.update_strings();
        self.update.stage = match result {
            Ok(CheckOutcome::UpToDate { latest }) => {
                if manual {
                    self.toast(Tone::Positive, strings.toast_latest, None, cx);
                }
                UpdateStage::Latest {
                    latest: latest.to_string(),
                    checked_unix: crate::app::remote::now_unix(),
                }
            }
            Ok(CheckOutcome::Available(release)) => UpdateStage::Available(release),
            Err(error) if manual => UpdateStage::Failed {
                step: UpdateStep::Check,
                error,
                release: None,
                update: None,
            },
            // Nobody asked: the window stays as it was, and the next start tries again.
            Err(_) => before,
        };
        cx.notify();
    }

    /// Downloads the release on offer; once it verifies, the install is asked for.
    pub(super) fn download_update(&mut self, cx: &mut Context<Self>) {
        let release = match &self.update.stage {
            UpdateStage::Available(release) => release.clone(),
            UpdateStage::Failed {
                release: Some(release),
                step: UpdateStep::Download | UpdateStep::Install,
                ..
            } => release.clone(),
            _ => return,
        };
        let Some(backend) = self.update.backend.clone() else {
            return;
        };
        if let Some(error) = release.unavailable_error() {
            self.update.stage = UpdateStage::Failed {
                step: UpdateStep::Download,
                error,
                release: Some(release),
                update: None,
            };
            cx.notify();
            return;
        }
        *self
            .update
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.update.stage = UpdateStage::Downloading {
            release: release.clone(),
            downloaded: 0,
            total: None,
        };
        let progress = Arc::clone(&self.update.progress);
        let asked = release.clone();
        let work = cx.background_executor().spawn(async move {
            backend.download(&asked, &mut |now| {
                *progress
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(now);
            })
        });
        // The download writes its progress where the window reads it; the bar is drawn
        // again while it runs.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_INTERVAL).await;
                let running = this
                    .update(cx, |view, cx| view.take_download_progress(cx))
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_update_download(release, result, cx)
            });
        })
        .detach();
        cx.notify();
    }

    /// Copies the download's progress into the stage; `false` once it is not running.
    fn take_download_progress(&mut self, cx: &mut Context<Self>) -> bool {
        let now = *self
            .update
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let UpdateStage::Downloading {
            downloaded, total, ..
        } = &mut self.update.stage
        else {
            return false;
        };
        if let Some(now) = now
            && (now.downloaded != *downloaded || now.total != *total)
        {
            *downloaded = now.downloaded;
            *total = now.total;
            cx.notify();
        }
        true
    }

    /// Takes in a finished download: a verified file is installed (after the question,
    /// when something is running); a refused one is said, and nothing was changed.
    fn finish_update_download(
        &mut self,
        release: Release,
        result: Result<DownloadedUpdate, UpdateError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(update) => {
                self.update.stage = UpdateStage::Ready { release, update };
                self.request_update_install(cx);
            }
            Err(error) => {
                self.update.stage = UpdateStage::Failed {
                    step: UpdateStep::Download,
                    error,
                    release: Some(release),
                    update: None,
                };
            }
        }
        cx.notify();
    }

    /// The sessions and transfers an install or a restart would stop.
    pub(super) fn active_work(&self) -> ActiveWork {
        let mut work = ActiveWork {
            transfers: self
                .transfers
                .transfers()
                .iter()
                .filter(|task| task.is_active())
                .count(),
            ..ActiveWork::default()
        };
        for session in self.workspace.sessions().items() {
            let phase = crate::app::workspace::SessionStatus::of(session).phase();
            if phase.is_over() || matches!(phase, crate::app::workspace::SessionPhase::NotStarted) {
                continue;
            }
            match session.kind() {
                SessionKind::Local { .. } | SessionKind::Remote { .. } => work.terminals += 1,
                SessionKind::Rdp { .. } | SessionKind::Vnc { .. } => work.desktops += 1,
                SessionKind::Serial { .. } => work.serials += 1,
            }
        }
        work
    }

    /// Installs the downloaded update, asking first when something would stop.
    pub(super) fn request_update_install(&mut self, cx: &mut Context<Self>) {
        let ready = matches!(self.update.stage, UpdateStage::Ready { .. })
            || matches!(
                self.update.stage,
                UpdateStage::Failed {
                    update: Some(_),
                    ..
                }
            );
        if !ready || self.update.waiting.is_some() {
            return;
        }
        if self.active_work().is_empty() {
            self.proceed_update(ConfirmPurpose::Install, cx);
        } else {
            self.update.confirm = Some(ConfirmPurpose::Install);
            cx.notify();
        }
    }

    /// Restarts into the installed version, asking first when something would stop.
    pub(super) fn request_update_restart(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.update.stage, UpdateStage::Restart { .. })
            || self.update.waiting.is_some()
        {
            return;
        }
        if self.active_work().is_empty() {
            self.proceed_update(ConfirmPurpose::Restart, cx);
        } else {
            self.update.confirm = Some(ConfirmPurpose::Restart);
            cx.notify();
        }
    }

    /// The answer "go on" to the question.
    pub(super) fn confirm_update(&mut self, cx: &mut Context<Self>) {
        let Some(purpose) = self.update.confirm.take() else {
            return;
        };
        self.restore_focus_pending = true;
        self.proceed_update(purpose, cx);
        cx.notify();
    }

    /// The answer "later": the update stays downloaded (or installed) and waits.
    pub(super) fn cancel_update_confirm(&mut self, cx: &mut Context<Self>) {
        if self.update.confirm.take().is_some() {
            self.restore_focus_pending = true;
            cx.notify();
        }
    }

    /// Whether the question is on screen.
    pub(super) fn update_confirm_open(&self) -> bool {
        self.update.confirm.is_some()
    }

    /// Saves the tabs, then installs or restarts once no saves are held back.
    fn proceed_update(&mut self, purpose: ConfirmPurpose, cx: &mut Context<Self>) {
        self.checkpoint_layout();
        if !self.data_writes_held() {
            self.carry_out_update(purpose, cx);
            return;
        }
        // A sync or an import is writing the data files, and the window's own saves
        // wait for it. Nothing is started - and the window does not exit - before they
        // are on disk.
        if self.update.waiting.is_some() {
            return;
        }
        self.update.waiting = Some(purpose);
        self.update.lost_before = self.held_writes_lost_count();
        let strings = self.update_strings();
        self.toast(
            Tone::Neutral,
            strings.install_waits,
            Some(strings.install_waits_detail.to_owned()),
            cx,
        );
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HELD_WRITES_POLL).await;
                let done = this
                    .update(cx, |view, cx| view.resume_update_after_held_writes(cx))
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Goes on with an update that waited for held saves, once they are written.
    /// Returns whether the waiting is over.
    fn resume_update_after_held_writes(&mut self, cx: &mut Context<Self>) -> bool {
        if self.data_writes_held() {
            return false;
        }
        let Some(purpose) = self.update.waiting.take() else {
            return true;
        };
        if self.held_writes_lost_count() != self.update.lost_before {
            // Some edits were not kept: the window stays, so they can be made again,
            // and the update keeps waiting where it was.
            let strings = self.update_strings();
            self.toast(Tone::Warning, strings.install_stopped, None, cx);
            cx.notify();
            return true;
        }
        self.carry_out_update(purpose, cx);
        true
    }

    /// Installs or restarts now.
    fn carry_out_update(&mut self, purpose: ConfirmPurpose, cx: &mut Context<Self>) {
        match purpose {
            ConfirmPurpose::Install => self.run_update_install(cx),
            ConfirmPurpose::Restart => self.run_update_restart(cx),
        }
    }

    /// Puts the downloaded file in place on the background executor.
    fn run_update_install(&mut self, cx: &mut Context<Self>) {
        let (release, update) = match &self.update.stage {
            UpdateStage::Ready { release, update } => (release.clone(), update.clone()),
            UpdateStage::Failed {
                release: Some(release),
                update: Some(update),
                ..
            } => (release.clone(), update.clone()),
            _ => return,
        };
        let Some(backend) = self.update.backend.clone() else {
            return;
        };
        self.update.stage = UpdateStage::Installing {
            release: release.clone(),
        };
        let file = update.clone();
        let work = cx
            .background_executor()
            .spawn(async move { backend.install(&file) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_update_install(release, update, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Takes in what the install did.
    fn finish_update_install(
        &mut self,
        release: Release,
        update: DownloadedUpdate,
        result: Result<InstallOutcome, UpdateError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(InstallOutcome::InstallerStarted) => {
                self.update.stage = UpdateStage::Installing { release };
                self.exit_for_update(cx);
            }
            Ok(InstallOutcome::RestartRequired(target)) => {
                self.update.stage = UpdateStage::Restart { release, target };
            }
            Ok(InstallOutcome::Manual {
                release_page,
                reason,
            }) => {
                // Nothing was changed: the release is offered on its page, with the
                // reason this copy could not install it.
                cx.open_url(&release_page);
                self.update.stage = UpdateStage::Available(Release {
                    asset: None,
                    unavailable: Some(Unavailable::ManualRoute(reason)),
                    release_page,
                    ..release
                });
            }
            Err(error) => {
                self.update.stage = UpdateStage::Failed {
                    step: UpdateStep::Install,
                    error,
                    release: Some(release),
                    update: Some(update),
                };
            }
        }
        cx.notify();
    }

    /// Starts the installed version, then exits.
    fn run_update_restart(&mut self, cx: &mut Context<Self>) {
        let UpdateStage::Restart { target, .. } = &self.update.stage else {
            return;
        };
        let Some(backend) = self.update.backend.clone() else {
            return;
        };
        match backend.relaunch(target) {
            Ok(()) => self.exit_for_update(cx),
            Err(error) => {
                let strings = self.update_strings();
                self.toast(
                    Tone::Danger,
                    strings.restart_failed,
                    Some(copy::fill(
                        strings.error_detail,
                        &[("reason", &error.to_string())],
                    )),
                    cx,
                );
            }
        }
    }

    /// Leaves the application so the installer or the new version takes over: the tabs
    /// are saved, every session is closed, and the exit goes the way every exit goes -
    /// waiting for saves still held back (R8).
    fn exit_for_update(&mut self, cx: &mut Context<Self>) {
        self.update.exiting = true;
        self.checkpoint_layout();
        self.app_close_confirmed = true;
        self.closing_app = false;
        self.close_all_sessions(cx);
        if !self.defer_exit_for_held_writes(PendingExit::Quit, cx) {
            cx.quit();
        }
        cx.notify();
    }

    /// "Try again" after a failure: the step that failed runs again.
    pub(super) fn retry_update(&mut self, cx: &mut Context<Self>) {
        let UpdateStage::Failed { step, update, .. } = &self.update.stage else {
            return;
        };
        match (step, update.is_some()) {
            (UpdateStep::Check, _) => self.check_for_updates(true, cx),
            (UpdateStep::Install, true) => self.request_update_install(cx),
            (UpdateStep::Download | UpdateStep::Install, _) => self.download_update(cx),
        }
    }

    /// Opens the page of the release on offer, or the list of releases.
    pub(super) fn open_update_release_page(&mut self, cx: &mut Context<Self>) {
        let url = self.update.stage.release().map_or_else(
            || shellune_update::RELEASES_URL.to_owned(),
            |release| release.release_page.clone(),
        );
        cx.open_url(&url);
    }

    /// Turns the automatic check on or off. It is a choice of this computer, written at
    /// once and not sent anywhere.
    pub(super) fn set_update_auto_check(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update.auto_check = on;
        if self.saved_preferences.is_some() {
            let now = self.current_preferences();
            if self.write_preferences_file(&now).is_ok() {
                self.saved_preferences = Some(now);
            }
        }
        cx.notify();
    }

    /// Opens the update card in the settings.
    pub(super) fn open_update_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select(Destination::Settings, window, cx);
        let index = Category::About
            .items()
            .iter()
            .position(|item| *item == super::settings::Item::Update)
            .unwrap_or_default();
        self.settings_select_item(Category::About, index, cx);
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// A line of text in the update card.
    fn update_line(
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

    /// A row of buttons in the update card.
    fn update_actions(&self, selector: &'static str) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::SNUG))
            .debug_selector(move || selector.to_owned())
    }

    /// The release's version, date and notes.
    fn update_release_rows(&self, release: &Release, rows: &mut Vec<Div>) {
        let strings = self.update_strings();
        let metrics = self.metrics();
        let mut head = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HALF))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::SNUG))
            .debug_selector(|| "settings.update.release".to_owned())
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(metrics.body_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "settings.update.available".to_owned())
                    .child(SharedString::from(copy::fill(
                        strings.available_title,
                        &[("version", &release.version.to_string())],
                    ))),
            );
        if let Some(date) = release
            .pub_date
            .as_deref()
            .map(|date| date.get(..10).unwrap_or(date))
            .filter(|date| !date.is_empty())
        {
            head = head.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(copy::fill(
                        strings.available_date,
                        &[("date", date)],
                    ))),
            );
        }
        rows.push(head);
        let notes = release
            .notes
            .as_deref()
            .map(str::trim)
            .filter(|notes| !notes.is_empty());
        let notes_block = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "settings.update.notes".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(strings.notes_label),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(SharedString::from(match notes {
                        Some(notes) => notes_excerpt(notes),
                        None => strings.no_notes.to_owned(),
                    })),
            );
        rows.push(notes_block);
    }

    /// The bar of a download: determinate when the total size is known, indeterminate
    /// (animated under full motion, still under reduced motion) when it is not.
    fn update_progress_bar(&self, downloaded: u64, total: Option<u64>) -> Div {
        let metrics = self.metrics();
        let progress = match progress_fraction(downloaded, total) {
            Some(fraction) => components::Progress::Determinate(fraction),
            None => components::Progress::Indeterminate,
        };
        div()
            .w_full()
            .px(metrics.scale(spacing::BASE))
            .debug_selector(|| "settings.update.progress".to_owned())
            .child(components::progress_bar(
                "settings.update.progress.bar",
                progress,
                metrics,
            ))
    }

    /// The update page: the automatic switch, the check, and the card of where the
    /// update is (R2, R3).
    pub(super) fn update_settings_page(&self, cx: &mut Context<Self>) -> Div {
        let strings = self.update_strings();
        let mut rows = vec![self.settings_row(
            "update-auto",
            strings.auto,
            strings.auto_hint,
            self.switch_control(
                "settings.update.auto",
                self.update.auto_check,
                cx,
                |view, on, cx| view.set_update_auto_check(on, cx),
            ),
        )];
        let stage = &self.update.stage;
        let can_check = !stage.busy()
            && !matches!(
                stage,
                UpdateStage::Ready { .. } | UpdateStage::Restart { .. }
            );
        // Only a check a person asked for turns the button into "Checking…"; the one at
        // start is said by the status line alone.
        let checking = matches!(stage, UpdateStage::Checking { manual: true });
        let mut check_row = self.update_actions("settings.update.check.row");
        let check_state = if checking {
            components::ControlState::Loading
        } else if can_check {
            components::ControlState::Resting
        } else {
            components::ControlState::Disabled
        };
        let check_button = components::compact_button_with_state(
            if checking {
                strings.checking
            } else {
                strings.check
            },
            Variant::Secondary,
            check_state,
            self.metrics(),
        )
        .id("settings.update.check")
        .debug_selector(|| "settings.update.check".to_owned())
        .when(can_check, |button| {
            button.on_click(cx.listener(|this, _event, _window, cx| {
                this.check_for_updates(true, cx);
            }))
        });
        check_row = check_row.child(check_button);
        rows.push(check_row);
        let version = self.version();
        match stage {
            UpdateStage::Idle => {
                rows.push(self.update_line(strings.status_idle, None, "settings.update.status"));
            }
            UpdateStage::Checking { .. } => {
                rows.push(self.update_line(strings.checking, None, "settings.update.status"));
            }
            UpdateStage::Latest {
                latest,
                checked_unix,
            } => rows.push(self.update_line(
                copy::fill(
                    strings.status_latest,
                    &[
                        ("version", latest),
                        ("date", &crate::app::hosts::format_unix(*checked_unix)),
                    ],
                ),
                Some(Tone::Positive),
                "settings.update.status",
            )),
            UpdateStage::Available(release) => {
                self.update_release_rows(release, &mut rows);
                let mut actions = self.update_actions("settings.update.actions");
                if let Some(reason) = release.unavailable {
                    rows.push(self.update_line(
                        unavailable_text(reason, strings),
                        Some(Tone::Warning),
                        "settings.update.reason",
                    ));
                } else {
                    actions = actions.child(self.text_button(
                        SharedString::from("settings.update.download"),
                        strings.download_install,
                        Variant::Primary,
                        cx,
                        |this, cx| this.download_update(cx),
                    ));
                }
                actions =
                    actions.child(self.release_page_button(release.unavailable.is_some(), cx));
                rows.push(actions);
            }
            UpdateStage::Downloading {
                release,
                downloaded,
                total,
            } => {
                self.update_release_rows(release, &mut rows);
                rows.push(self.update_line(strings.downloading, None, "settings.update.status"));
                rows.push(self.update_progress_bar(*downloaded, *total));
                rows.push(self.update_line(
                    progress_text(*downloaded, *total, strings),
                    None,
                    "settings.update.progress.text",
                ));
            }
            UpdateStage::Ready { release, .. } => {
                self.update_release_rows(release, &mut rows);
                rows.push(self.update_line(
                    strings.ready,
                    Some(Tone::Positive),
                    "settings.update.status",
                ));
                if matches!(
                    self.update.route,
                    Some(InstallRoute::WindowsInstaller { .. })
                ) {
                    rows.push(self.update_line(
                        strings.installer_note,
                        None,
                        "settings.update.installer",
                    ));
                }
                rows.push(
                    self.update_actions("settings.update.actions")
                        .child(self.text_button(
                            SharedString::from("settings.update.install"),
                            strings.install,
                            Variant::Primary,
                            cx,
                            |this, cx| this.request_update_install(cx),
                        )),
                );
            }
            UpdateStage::Installing { release } => {
                self.update_release_rows(release, &mut rows);
                rows.push(self.update_line(strings.installing, None, "settings.update.status"));
            }
            UpdateStage::Restart { release, .. } => {
                rows.push(self.update_line(
                    copy::fill(
                        strings.restart_ready,
                        &[("version", &release.version.to_string())],
                    ),
                    Some(Tone::Positive),
                    "settings.update.status",
                ));
                rows.push(
                    self.update_actions("settings.update.actions")
                        .child(self.text_button(
                            SharedString::from("settings.update.restart"),
                            strings.restart,
                            Variant::Primary,
                            cx,
                            |this, cx| this.request_update_restart(cx),
                        )),
                );
            }
            UpdateStage::Failed {
                step,
                error,
                release,
                ..
            } => {
                if let Some(release) = release {
                    self.update_release_rows(release, &mut rows);
                }
                let heading = match step {
                    UpdateStep::Check => strings.failed_check,
                    UpdateStep::Download => strings.failed_download,
                    UpdateStep::Install => strings.failed_install,
                };
                rows.push(
                    self.update_line(heading, Some(Tone::Danger), "settings.update.status")
                        .font_weight(FontWeight::MEDIUM),
                );
                rows.push(self.update_line(
                    error_text(error, *step, strings),
                    Some(Tone::Danger),
                    "settings.update.error",
                ));
                rows.push(self.update_line(
                    copy::fill(strings.unchanged, &[("version", version)]),
                    None,
                    "settings.update.unchanged",
                ));
                let mut actions =
                    self.update_actions("settings.update.actions")
                        .child(self.text_button(
                            SharedString::from("settings.update.retry"),
                            strings.retry,
                            Variant::Secondary,
                            cx,
                            |this, cx| this.retry_update(cx),
                        ));
                if release.is_some() {
                    actions = actions.child(self.release_page_button(false, cx));
                }
                rows.push(actions);
            }
        }
        self.settings_card("update", rows)
    }

    /// "Open release page", the main action when it is the only way to update.
    fn release_page_button(&self, primary: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        self.text_button(
            SharedString::from("settings.update.release-page"),
            self.update_strings().release_page,
            if primary {
                Variant::Primary
            } else {
                Variant::Ghost
            },
            cx,
            |this, cx| this.open_update_release_page(cx),
        )
    }

    /// What the top row says while a newer release is known: the chip's words.
    pub(super) fn update_notice(&self) -> Option<String> {
        let release = self.update.stage.release()?;
        Some(copy::fill(
            self.update_strings().chip,
            &[("version", &release.version.to_string())],
        ))
    }

    /// The chip in the top bar while a newer release is known; a press opens the card.
    ///
    /// The row decides its form ([`ShelluneView::topbar_notice_form`]): the words when
    /// there is room for them, an accent arrow the size of the other top-bar buttons
    /// with the words in its tooltip when there is not, and nothing of its own in a
    /// compact row, where the settings button carries it (R2, R9).
    pub(super) fn update_topbar_chip(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let label = self.update_notice()?;
        let strings = self.update_strings();
        let metrics = self.metrics();
        let open = cx.listener(|this, _event: &gpui::ClickEvent, window, cx| {
            this.open_update_settings(window, cx);
        });
        match self.topbar_notice_form() {
            NoticeForm::Folded => None,
            NoticeForm::Icon => Some(
                notice_icon(
                    "topbar.update",
                    Icon::ArrowUp,
                    Tone::Accent,
                    format!("{label} \u{b7} {}", strings.chip_hint),
                    metrics,
                )
                .on_click(open),
            ),
            NoticeForm::Words => Some(
                notice_words("topbar.update", label, Tone::Accent, metrics)
                    .tooltip(components::tooltip(SharedString::from(strings.chip_hint)))
                    .on_click(open),
            ),
        }
    }

    /// The question before an install or a restart, listing what would stop.
    pub(super) fn update_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let purpose = self.update.confirm?;
        let strings = self.update_strings();
        let metrics = self.metrics();
        let title = match purpose {
            ConfirmPurpose::Install => strings.confirm_title,
            ConfirmPurpose::Restart => strings.confirm_restart_title,
        };
        let (header, subtitle) = self.modal_header(
            Some(Icon::Refresh),
            title.to_owned(),
            None,
            "update.confirm.close",
            cx,
            |this, cx| this.cancel_update_confirm(cx),
        );
        let mut list = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::CHROME))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "update.confirm.list".to_owned());
        for (index, line) in self.active_work().lines(strings).into_iter().enumerate() {
            list = list.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(move || format!("update.confirm.line.{index}"))
                    .child(SharedString::from(line)),
            );
        }
        let body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "update.confirm.body".to_owned())
            .child(
                components::banner(Tone::Warning, metrics)
                    .whitespace_normal()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "update.confirm.intro".to_owned())
                    .child(strings.confirm_intro),
            )
            .child(list)
            .capture_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.cancel_update_confirm(cx);
                    }
                }),
            );
        let footer = components::modal_footer(metrics)
            .child(self.text_button(
                SharedString::from("update.confirm.cancel"),
                strings.later,
                Variant::Ghost,
                cx,
                |this, cx| this.cancel_update_confirm(cx),
            ))
            .child(self.text_button(
                SharedString::from("update.confirm.ok"),
                match purpose {
                    ConfirmPurpose::Install => strings.confirm_install,
                    ConfirmPurpose::Restart => strings.confirm_restart,
                },
                Variant::Danger,
                cx,
                |this, cx| this.confirm_update(cx),
            ));
        Some(self.modal(
            window,
            "update.confirm.dialog",
            480.0,
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
    /// Builds the shell over `store` with an update backend the test holds: no request
    /// leaves the machine and no installer is started unless the backend does it.
    #[cfg(test)]
    pub fn with_update_for_test(
        cx: &mut Context<Self>,
        workspace: crate::app::workspace::Workspace,
        store: shellune_storage::HostStore,
        backend: Arc<dyn UpdateBackend>,
    ) -> Self {
        let mut view = Self::with_secrets_for_test(
            cx,
            workspace,
            store,
            Arc::new(shellune_storage::MemorySecrets::new()),
        );
        view.use_update_backend_for_test(backend, cx);
        view
    }

    /// Hands the window an update backend after it was built, and starts it as a
    /// window that just opened.
    #[cfg(test)]
    pub fn use_update_backend_for_test(
        &mut self,
        backend: Arc<dyn UpdateBackend>,
        cx: &mut Context<Self>,
    ) {
        self.update = UpdateCenter::new(Ok(Some(backend)));
        self.update.auto_check = self
            .saved_preferences
            .as_ref()
            .and_then(|saved| saved.auto_check_updates)
            .unwrap_or(true);
        self.start_updates(cx);
    }

    /// The stage's name, how many checks ended, and whether the window asked to exit.
    #[cfg(test)]
    #[must_use]
    pub fn update_progress_for_test(&self) -> (&'static str, usize, bool) {
        (
            self.update.stage.name(),
            self.update.checks,
            self.update.exiting,
        )
    }

    /// What the card says about a failure: the sentence of the error.
    #[cfg(test)]
    #[must_use]
    pub fn update_error_for_test(&self) -> Option<String> {
        match &self.update.stage {
            UpdateStage::Failed { step, error, .. } => {
                Some(error_text(error, *step, self.update_strings()))
            }
            _ => None,
        }
    }

    /// Why the release on offer is only on its page.
    #[cfg(test)]
    #[must_use]
    pub fn update_unavailable_for_test(&self) -> Option<String> {
        match &self.update.stage {
            UpdateStage::Available(release) => release
                .unavailable
                .map(|reason| unavailable_text(reason, self.update_strings())),
            _ => None,
        }
    }

    /// The question's lines, when it is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn update_confirm_for_test(&self) -> Option<Vec<String>> {
        self.update
            .confirm
            .map(|_| self.active_work().lines(self.update_strings()))
    }

    /// Whether an update waits for held saves.
    #[cfg(test)]
    #[must_use]
    pub fn update_waiting_for_test(&self) -> bool {
        self.update.waiting.is_some()
    }

    /// Puts a newer release on offer without asking any server, so a test can see what
    /// the top row does with its notice.
    #[cfg(test)]
    pub fn offer_update_for_test(&mut self, cx: &mut Context<Self>) {
        self.update.backend = None;
        self.update.stage = UpdateStage::Available(capture_release());
        cx.notify();
    }

    /// Whether the automatic check is on.
    #[cfg(test)]
    #[must_use]
    pub fn update_auto_check_for_test(&self) -> bool {
        self.update.auto_check
    }

    /// Opens the update card with a fixed release on offer, for a capture: nothing is
    /// sent, and nothing can be downloaded.
    #[cfg(feature = "ui-capture")]
    pub fn capture_update_available(&mut self, cx: &mut Context<Self>) {
        self.update.backend = None;
        self.update.stage = UpdateStage::Available(capture_release());
        self.settings_reveal(Category::About);
        cx.notify();
    }

    /// Puts the capture release on offer without opening anything, so the shot shows
    /// the top row's notice over whatever page is on screen.
    #[cfg(feature = "ui-capture")]
    pub fn capture_update_notice(&mut self, cx: &mut Context<Self>) {
        self.update.backend = None;
        self.update.stage = UpdateStage::Available(capture_release());
        cx.notify();
    }

    /// Opens the question before an install over the update card, counting the
    /// sessions the window holds: nothing is installed.
    #[cfg(feature = "ui-capture")]
    pub fn capture_update_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select(Destination::Settings, window, cx);
        self.capture_settings_page(super::settings::Item::Update, cx);
        self.capture_update_available(cx);
        self.update.confirm = Some(ConfirmPurpose::Install);
        cx.notify();
    }

    /// How many lines the question before an install lists, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_update_confirm_lines(&self) -> usize {
        self.active_work().lines(self.update_strings()).len()
    }

    /// Whether the update card shows a release on offer, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_update_stage(&self) -> &'static str {
        self.update.stage.name()
    }

    /// Whether the question before an install is on screen, for a capture.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_update_confirm_open(&self) -> bool {
        self.update_confirm_open()
    }
}

/// The release a capture shows: a fixed version one minor above this one, with notes.
#[cfg(any(test, feature = "ui-capture"))]
fn capture_release() -> Release {
    let current = shellune_update::parse_version(shellune_update::CURRENT_VERSION)
        .unwrap_or_else(|_| shellune_update::parse_version("0.0.0").expect("a version"));
    let version =
        shellune_update::parse_version(&format!("{}.{}.0", current.major, current.minor + 1))
            .unwrap_or(current);
    Release {
        release_page: shellune_update::release_page(&version),
        version,
        notes: Some(
            "- Home page: quick connect, recent and favourite hosts, groups\n\
             - Backup export and import, with encrypted passwords\n\
             - WebDAV sync of hosts, commands, forwards and preferences\n\
             - Signed self-update from the about page"
                .to_owned(),
        ),
        pub_date: Some("2026-11-20T08:00:00Z".to_owned()),
        asset: Some((
            shellune_update::PlatformKey::WindowsX86_64Portable,
            shellune_update::Asset {
                url: "https://example.invalid/Shellune-portable.zip".to_owned(),
                signature: "capture".to_owned(),
            },
        )),
        unavailable: None,
    }
}
