//! The batch panel as it is drawn: the preview before anything is confirmed, and the
//! run with its per-host state, output and timeline (R19, R21).
//!
//! Both surfaces are one centred dialog, because that is what they are: a decision about
//! a list of hosts, and then the report of what that decision did. The model behind them
//! is [crate::app::batch], which holds no rendering and cannot dial.
//!
//! Three properties are visible in the layout rather than only in the model:
//!
//! * the preview names the number of hosts before the commands, because the number is
//!   the part a person has to get right;
//! * a run row says the state of every step, its exit status and its reason, so "which
//!   host failed at which step" is read rather than inferred;
//! * cancel is not drawn as a stop: while the remote command has not ended the page says
//!   cancelling, and the run's own end is what turns it into cancelled.

use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, SharedString, Stateful, Window, div, px};
use shellune_core::{BatchTarget, FailurePolicy, HostId, HostState, RunState, StepState};

use super::ShelluneView;
use super::shell::{ModalContent, NoticeForm};
use crate::app::batch::{BatchHostFilter, BatchRunPage, MAX_PREVIEW_ROWS};
use crate::app::components::{self, Variant};
use crate::app::copy::{self, UxStrings};
use crate::app::hosts::SelectionScope;
use crate::app::icons::Icon;
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};

/// Width of the batch dialog before the minimum window narrows it.
///
/// Wide enough for a host row with its steps on one line and an output pane beside the
/// list; the dialog falls back to the viewport minus its margin at 720 pixels.
const BATCH_DIALOG_WIDTH: f32 = 880.0;

/// The tallest the saved tasks and the stored runs grow before they scroll.
const LIST_MAX_HEIGHT: f32 = 320.0;
/// The tallest the hosts of a stored run grow before they scroll.
const HOSTS_MAX_HEIGHT: f32 = 160.0;
/// The height of a stored run's output.
const OUTPUT_HEIGHT: f32 = 140.0;
/// The tallest the targets of the editor grow before they scroll.
const TARGETS_MAX_HEIGHT: f32 = 140.0;
/// The width of a step's number before its command.
const STEP_NUMBER_WIDTH: f32 = 20.0;
/// The width of the hosts-at-once field.
const CONCURRENCY_WIDTH: f32 = 72.0;
/// The least width of the task name field.
const NAME_MIN_WIDTH: f32 = 160.0;
/// The least width of the running hosts column before the output moves under it.
const RUN_HOSTS_MIN_WIDTH: f32 = 320.0;
/// The tallest the running hosts column grows before it scrolls.
const RUN_HOSTS_MAX_HEIGHT: f32 = 300.0;
/// The least width of the output column of a run.
const RUN_SIDE_MIN_WIDTH: f32 = 300.0;
/// The height of a running host's output.
const RUN_OUTPUT_HEIGHT: f32 = 170.0;

/// How many host rows a run page lists before it counts the rest.
///
/// A task can hold thousands of hosts. The count above the list is exact, so a row that
/// is not drawn is still counted; drawing thousands of rows inside a dialog would make
/// the page unusable instead.
const MAX_RUN_ROWS: usize = 200;

/// How many saved tasks a library lists before it counts the rest.
///
/// The library is a list of definitions a person picks from, not a table to scroll
/// through: the count above it is exact, and a row that is not drawn is still counted,
/// so thousands of saved tasks stay inside one dialog (R5).
const MAX_LIBRARY_ROWS: usize = 60;

/// How many stored runs a history lists before it counts the rest.
const MAX_HISTORY_ROWS: usize = 60;

/// How many hosts of one stored run its detail lists before it counts the rest.
const MAX_HISTORY_HOSTS: usize = 120;

/// The states the history can be narrowed to, in the order the filter lists them.
///
/// A run that is still going is not here: it is the run this window is driving, and it
/// has a surface of its own.
const HISTORY_FILTERS: [RunState; 4] = [
    RunState::Completed,
    RunState::Failed,
    RunState::Cancelled,
    RunState::Interrupted,
];

/// The debug selector of one filter value.
fn history_filter_selector(state: RunState) -> &'static str {
    match state {
        RunState::Completed => "batch.history.filter.completed",
        RunState::Failed => "batch.history.filter.failed",
        RunState::Cancelled => "batch.history.filter.cancelled",
        RunState::Interrupted => "batch.history.filter.interrupted",
        _ => "batch.history.filter.other",
    }
}

/// Removes every credential-shaped word from one sentence of a batch report.
///
/// The session sanitiser knows two shapes: the user information of a URL, and a
/// password-style pair such as password=... A batch reason can also arrive as
/// user:secret@host with no scheme in front of it, because that is how a sign-in failure
/// is often worded. The word in front of the @ is dropped when it holds a colon and the
/// host is kept, so a reader still sees what refused without seeing what was tried.
pub(crate) fn redact_batch_text(text: &str) -> String {
    let cleaned = crate::app::workspace::sanitize_diagnostic(text);
    let mut out = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();
    while let Some(at) = rest.find('@') {
        let (head, tail) = rest.split_at(at);
        let start = head
            .rfind([' ', '\t', '(', '"', '\'', ',', ';'])
            .map_or(0, |index| index + 1);
        let candidate = &head[start..];
        if candidate.contains(':') {
            out.push_str(&head[..start]);
        } else {
            out.push_str(head);
            out.push('@');
        }
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}
/// The diagnostics of one run as text, for a bug report.
///
/// It holds what a reader needs to see what the run did - the identity, the state, the
/// hosts with their addresses, the state and exit status of every step and the reason a
/// step gave - and nothing a person would not paste into a ticket: no password, no
/// passphrase, no key, and no user name. The whole text still goes through
/// [crate::app::workspace::sanitize_diagnostic] before it leaves, because a reason the
/// server chose can be anything at all (R13, R22).
pub(crate) fn batch_diagnostics(run: &shellune_core::BatchRun) -> String {
    let ux = copy::ux();
    let mut text = format!("shellune batch run {}\n", run.id());
    text.push_str(&format!(
        "state: {} · started: {} · hosts: {} · steps: {} · at once: {}\n",
        run_state_text(run.state(), ux),
        run.started_at_unix(),
        run.hosts().len(),
        run.task().steps().len(),
        run.task().concurrency()
    ));
    text.push_str(&format!(
        "task: {}\n",
        run.task().name().unwrap_or("-").to_owned()
    ));
    for host in run.hosts() {
        text.push_str(&format!(
            "host {} {}:{} · {}\n",
            host.target().label(),
            host.target().address(),
            host.target().port(),
            host_state_text(host.state(), ux)
        ));
        for step in host.steps() {
            let exit = step
                .exit()
                .map(|code| {
                    format!(
                        " {}",
                        copy::fill(ux.batch_exit, &[("code", &code.to_string())])
                    )
                })
                .unwrap_or_default();
            let detail = step
                .detail()
                .map(|reason| format!(" · {reason}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  step {} {}{exit}{detail}\n",
                step.index() + 1,
                step_state_text(step.state(), ux)
            ));
        }
        if let Some(reason) = host.detail() {
            text.push_str(&format!("  host detail: {reason}\n"));
        }
    }
    redact_batch_text(&text)
}

impl ShelluneView {
    // -----------------------------------------------------------------------
    // Opening, confirming and closing
    // -----------------------------------------------------------------------

    /// The hosts a batch task would run on, from the selection when there is one and
    /// from the chosen scope when there is not.
    ///
    /// The scope means the same thing here as in the host list (R18): "current results"
    /// is what the search and the filters keep, "all hosts" is the whole book. A host
    /// that cannot be snapshotted is kept aside with its reason instead of being dropped
    /// silently.
    fn batch_targets(&self) -> (Vec<BatchTarget>, Vec<(String, String)>) {
        let defaults = self.connection_defaults();
        let scope = self.batch.scope();
        let list = self
            .hosts
            .list(&self.book, &self.groups, &|id| self.host_is_open(id));
        let in_scope = list.ids_in_scope(scope);
        let wanted: Vec<HostId> = if self.hosts.selected().is_empty() {
            in_scope.to_vec()
        } else {
            self.hosts
                .selected()
                .ids()
                .into_iter()
                .filter(|id| in_scope.contains(id))
                .collect()
        };
        let mut targets = Vec::new();
        let mut refused = Vec::new();
        for id in wanted {
            let Some(host) = self.book.host(id) else {
                refused.push((
                    id.to_string(),
                    copy::ux().batch_reason_missing_host.to_owned(),
                ));
                continue;
            };
            match BatchTarget::snapshot(host, defaults) {
                Ok(target) => targets.push(target),
                Err(problem) => refused.push((host.name().to_owned(), problem.to_string())),
            }
        }
        (targets, refused)
    }

    /// Opens the preview for what the selection and the scope currently mean.
    pub(super) fn open_batch_preview(&mut self, cx: &mut Context<Self>) {
        self.batch.set_scope(self.hosts.scope());
        let (targets, refused) = self.batch_targets();
        let store = Some(shellune_storage::BatchStore::at(self.store.root()));
        self.batch
            .open_preview(cx, self.hosts.scope(), targets, refused, store);
        cx.notify();
    }

    /// Changes the scope the preview takes its targets from, and re-snapshots them.
    pub(super) fn select_batch_scope(&mut self, scope: SelectionScope, cx: &mut Context<Self>) {
        self.batch.set_scope(scope);
        let (targets, refused) = self.batch_targets();
        self.batch.retarget(targets, refused);
        cx.notify();
    }

    /// Confirms the preview and starts the run.
    ///
    /// The routes are built from this window's own book, trust file and credential
    /// store, which is what makes a confirmed batch reach the same servers an
    /// interactive connection does. Nothing here dials: the runner does, one host at a
    /// time, and only after this returns.
    pub(super) fn confirm_batch(&mut self, cx: &mut Context<Self>) {
        // The run signs in the way a single connection would: a target that
        // names a managed key or a shared password of the vault resolves its
        // body from the same unlocked copy the window holds (R1).
        let routes = crate::app::batch::StoredRoutes::new(
            &self.book,
            &self.trust_book,
            self.secrets.clone(),
        )
        .with_vault(self.security.content().cloned());
        let factory: std::sync::Arc<dyn shellune_ssh::batch::TransportFactory> =
            std::sync::Arc::new(shellune_ssh::batch::SshTransports::new(
                std::sync::Arc::new(routes),
            ));
        if self.batch.confirm(cx, factory).is_err() {
            cx.notify();
            return;
        }
        self.hosts.clear_selection();
        self.watch_batch(cx);
        cx.notify();
    }

    /// Offers a retry of the run on screen, as a new preview of the work still owed.
    pub(super) fn retry_batch(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.batch.run().map(|page| page.run().clone()) else {
            return;
        };
        match self.batch.open_retry(cx, &run) {
            Ok(()) => {}
            Err(problem) => self.batch.set_problem(problem.to_string()),
        }
        cx.notify();
    }

    /// Puts the panel away.
    pub(super) fn close_batch(&mut self, cx: &mut Context<Self>) {
        if self.batch.close() {
            self.restore_focus_pending = true;
        }
        cx.notify();
    }

    /// Asks the run to stop. The page reports cancelling until the commands have ended.
    pub(super) fn cancel_batch(&mut self, cx: &mut Context<Self>) {
        self.batch.cancel();
        self.watch_batch(cx);
        cx.notify();
    }

    /// Stops starting new steps.
    pub(super) fn pause_batch(&mut self, cx: &mut Context<Self>) {
        self.batch.pause();
        self.watch_batch(cx);
        cx.notify();
    }

    /// Continues a paused run.
    pub(super) fn resume_batch(&mut self, cx: &mut Context<Self>) {
        self.batch.resume();
        self.watch_batch(cx);
        cx.notify();
    }

    /// Answers the question one host is waiting on.
    pub(super) fn answer_batch_trust(
        &mut self,
        host: HostId,
        accept: bool,
        cx: &mut Context<Self>,
    ) {
        self.batch.answer_trust(host, accept);
        self.watch_batch(cx);
        cx.notify();
    }

    /// Keeps asking the run what happened until it has ended.
    ///
    /// The poll is the same shape the connectors use: a timer, then one read of what the
    /// runner reported. It stops when the panel has nothing left to watch.
    pub(super) fn watch_batch(&mut self, cx: &mut Context<Self>) {
        if !self.batch.is_watching() || self.batch_polling {
            return;
        }
        self.batch_polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(crate::app::batch::BATCH_POLL_INTERVAL)
                    .await;
                match this.update(cx, |view, cx| view.poll_batch(cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Reads what the batch runner reported, returning whether to keep watching.
    pub(super) fn poll_batch(&mut self, cx: &mut Context<Self>) -> bool {
        let was_watching = self.batch.is_watching();
        let watching = self.batch.pump();
        if !watching {
            self.batch_polling = false;
            // The run this window was driving just ended, and its summary was written as
            // it ended. A history page that is already on screen would otherwise keep
            // listing the runs from before it, so staying on it does not mean a stale
            // list: it is re-read once, when the run joins it.
            if was_watching && self.batch.view() == crate::app::batch::BatchCenterView::History {
                self.batch.refresh_history();
            }
        }
        cx.notify();
        watching
    }

    // -----------------------------------------------------------------------
    // The task centre: saved tasks, this run, and the stored history
    // (R11, R12, R13)
    // -----------------------------------------------------------------------

    /// Gives the task centre the store the rest of the window uses.
    ///
    /// The library and the history are read from it, so both can be listed before a
    /// preview has ever been opened. Nothing is read here.
    pub(super) fn attach_batch_store(&mut self) {
        let store = shellune_storage::BatchStore::at(self.store.root());
        self.batch.attach_store(store);
    }

    /// Opens the saved definitions.
    pub(super) fn open_batch_library(&mut self, cx: &mut Context<Self>) {
        self.attach_batch_store();
        self.batch.show_library();
        cx.notify();
    }

    /// Opens the stored runs of the past.
    pub(super) fn open_batch_history(&mut self, cx: &mut Context<Self>) {
        self.attach_batch_store();
        self.batch.show_history();
        cx.notify();
    }

    /// Switches the task centre to one of its four surfaces.
    ///
    /// Reading the library or the history starts nothing; going back to the run does not
    /// restart it either, because the runner never stopped. A surface that has nothing
    /// to show is refused by the model and the page stays where it is.
    pub(super) fn show_batch_surface(
        &mut self,
        surface: crate::app::batch::BatchCenterView,
        cx: &mut Context<Self>,
    ) {
        use crate::app::batch::BatchCenterView;
        match surface {
            BatchCenterView::Library => {
                self.open_batch_library(cx);
                return;
            }
            BatchCenterView::History => {
                self.open_batch_history(cx);
                return;
            }
            BatchCenterView::ActiveRun => {
                self.batch.show_active_run();
            }
            BatchCenterView::Editor => {
                self.batch.show_editor();
            }
            BatchCenterView::Closed => {
                self.batch.hide();
            }
        }
        cx.notify();
    }

    /// Puts the task centre away.
    ///
    /// The run page of a run that has not ended stays: it is the only place its state is
    /// visible, and leaving it is the background action, which says what it does (R13).
    /// Every other surface is a list or a question, so it goes.
    pub(super) fn dismiss_batch(&mut self, cx: &mut Context<Self>) {
        use crate::app::batch::BatchCenterView;
        // Only a run that is still going keeps its page. A run that has ended is in the
        // history, so its page can be dropped like any other surface: gating this on
        // "a page exists" would leave a finished run with no way out through Escape or
        // the header's close button.
        if self.batch.is_watching() && self.batch.view() == BatchCenterView::ActiveRun {
            return;
        }
        if self.batch.close() || self.batch.hide() {
            self.restore_focus_pending = true;
        }
        cx.notify();
    }

    /// Leaves the run page with the run still going, and says where it came back from.
    pub(super) fn background_batch(&mut self, cx: &mut Context<Self>) {
        if self.batch.hide() {
            self.restore_focus_pending = true;
            let ux = copy::ux();
            self.toast(
                Tone::Accent,
                ux.batch_run_background,
                Some(ux.batch_run_background_hint.to_owned()),
                cx,
            );
        }
        cx.notify();
    }

    /// Opens one saved definition in the editor under the identity it was saved with.
    pub(super) fn edit_saved_batch(
        &mut self,
        id: shellune_core::BatchTaskId,
        cx: &mut Context<Self>,
    ) {
        if let Err(problem) = self.batch.edit_saved(cx, id) {
            self.batch.set_problem(problem);
        }
        cx.notify();
    }

    /// Deletes one saved definition, after the question was answered.
    pub(super) fn delete_saved_batch(
        &mut self,
        id: shellune_core::BatchTaskId,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.batch.delete_saved(id);
        self.batch.clear_delete_asked();
        match outcome {
            Ok(()) => {
                let title = copy::ux().batch_library_deleted.to_owned();
                self.toast(Tone::Positive, title, None, cx);
            }
            Err(problem) => self.batch.set_problem(problem),
        }
        cx.notify();
    }

    /// Saves the definition being edited, without running it.
    pub(super) fn save_batch_draft(&mut self, cx: &mut Context<Self>) {
        let ux = copy::ux();
        // A refusal is already on the page as its problem sentence, so only the success
        // is announced here: what was saved, and that nothing ran.
        if self.batch.save_draft(cx).is_ok() {
            let title = ux.batch_saved.to_owned();
            self.toast(Tone::Positive, title, None, cx);
        }
        cx.notify();
    }

    /// Opens one stored run's commands, exit statuses and timeline.
    pub(super) fn open_stored_batch(
        &mut self,
        id: shellune_core::BatchRunId,
        cx: &mut Context<Self>,
    ) {
        if let Err(problem) = self.batch.open_history_entry(id) {
            self.batch.set_problem(problem);
        }
        cx.notify();
    }

    /// Reads one host's output of the stored run that is open.
    pub(super) fn select_stored_host(
        &mut self,
        host: shellune_core::HostId,
        cx: &mut Context<Self>,
    ) {
        if let Err(problem) = self.batch.open_stored_host(host) {
            self.batch.set_problem(problem);
        }
        cx.notify();
    }

    /// Copies the diagnostics of one run, with the credentials taken out.
    ///
    /// The text is the run's own record - the hosts, the states, the exit statuses and
    /// the reasons - passed through the same sanitiser a failed session uses, so a
    /// password that a server echoed back cannot leave the application through it (R13).
    pub(super) fn copy_batch_diagnostics(
        &mut self,
        run: &shellune_core::BatchRun,
        cx: &mut Context<Self>,
    ) {
        let text = batch_diagnostics(run);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        let title = copy::ux().batch_diagnostics_copied.to_owned();
        self.toast(Tone::Positive, title, None, cx);
        cx.notify();
    }

    /// The keys the task centre's lists read while they are on screen.
    ///
    /// Up and Down move the mark, Enter opens the marked row: a saved task opens in the
    /// editor, a stored run opens its detail. The editor is deliberately not here - a
    /// key pressed there belongs to the text field the user is typing in - so this
    /// answers false for it and the key goes on its way (R4).
    ///
    /// Returns whether the key was spent.
    pub(super) fn batch_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::app::batch::BatchCenterView;
        match self.batch.view() {
            BatchCenterView::Library => {
                // The mark can only be on a row that was drawn: a list longer than the
                // rows on screen says how many it left out, and Enter may not open one
                // of those, because a person cannot see what they would be opening.
                let count = self.batch.library().tasks().len().min(MAX_LIBRARY_ROWS);
                let current = self.batch.library_selected();
                match event.keystroke.key.as_str() {
                    "down" if count > 0 => {
                        let next = crate::app::palette::step(current, 1, count);
                        self.batch.select_library_row(next);
                        cx.notify();
                        true
                    }
                    "up" if count > 0 => {
                        let next = crate::app::palette::step(current, -1, count);
                        self.batch.select_library_row(next);
                        cx.notify();
                        true
                    }
                    "enter" if count > 0 => {
                        let index = current.min(count - 1);
                        if let Some(task) = self.batch.library().tasks().get(index) {
                            let id = task.id();
                            self.edit_saved_batch(id, cx);
                        }
                        true
                    }
                    _ => false,
                }
            }
            BatchCenterView::ActiveRun => match event.keystroke.key.as_str() {
                "down" if !event.keystroke.modifiers.modified() => {
                    let moved = self.batch.step_run_host(1, MAX_RUN_ROWS);
                    if moved {
                        cx.notify();
                    }
                    moved
                }
                "up" if !event.keystroke.modifiers.modified() => {
                    let moved = self.batch.step_run_host(-1, MAX_RUN_ROWS);
                    if moved {
                        cx.notify();
                    }
                    moved
                }
                _ => false,
            },
            BatchCenterView::History => {
                if self.batch.stored_open().is_some() {
                    if event.keystroke.modifiers.modified() {
                        return false;
                    }
                    return match event.keystroke.key.as_str() {
                        "down" => {
                            if let Some(host) = self.batch.next_stored_host(1, MAX_HISTORY_HOSTS) {
                                self.select_stored_host(host, cx);
                                true
                            } else {
                                false
                            }
                        }
                        "up" => {
                            if let Some(host) = self.batch.next_stored_host(-1, MAX_HISTORY_HOSTS) {
                                self.select_stored_host(host, cx);
                                true
                            } else {
                                false
                            }
                        }
                        _ => false,
                    };
                }
                let visible = self.batch.history_visible();
                // The same rule as the library: the mark stays on a row that was drawn,
                // so Enter never opens a run the list did not show.
                let count = visible.len().min(MAX_HISTORY_ROWS);
                let current = self.batch.history_selected();
                match event.keystroke.key.as_str() {
                    "down" if count > 0 => {
                        let next = crate::app::palette::step(current, 1, count);
                        self.batch.select_history_row(next);
                        cx.notify();
                        true
                    }
                    "up" if count > 0 => {
                        let next = crate::app::palette::step(current, -1, count);
                        self.batch.select_history_row(next);
                        cx.notify();
                        true
                    }
                    "enter" if count > 0 => {
                        let index = current.min(count - 1);
                        let id = visible.get(index).map(|entry| entry.id());
                        if let Some(id) = id {
                            self.open_stored_batch(id, cx);
                        }
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// The control the top bar draws for the task centre.
    ///
    /// Two things, and they say different things: an entry that opens the saved tasks,
    /// and - only while this window is driving a run - the run's own state, which
    /// returns to the same page with its selection and output still in place (R11,
    /// R13). The state is the model's, so an indicator cannot show a run that ended.
    pub(in crate::app) fn batch_topbar_control(
        &self,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let ux = copy::ux();
        let metrics = self.metrics();
        let notice = self.batch_notice();
        let form = self.topbar_notice_form();
        let home = self.home_is_shown();
        let mut controls = Vec::new();
        if home {
            let selected = self.batch.view().is_open();
            // A compact row has no room for the run's own indicator, so the entry carries
            // it: a mark in the run's tone, the run's state in its tooltip, and a press that
            // returns to the run - whose page switches to the saved tasks too (R2, R9, R13).
            let folded = notice.clone().filter(|_| form == NoticeForm::Folded);
            let tooltip = match &folded {
                Some((label, _)) => format!("{label} · {}", ux.batch_run_background_hint),
                None => format!("{} · {}", ux.batch_center_open, ux.batch_center_open_hint),
            };
            let entry = components::icon_only_button(Icon::Layers, metrics)
                .relative()
                .id("topbar.batch")
                .debug_selector(|| "topbar.batch".to_owned())
                .tooltip(components::tooltip(SharedString::from(tooltip)))
                .when(selected, |button| {
                    button.bg(theme::swatch(palette::SURFACE_SELECTED))
                })
                .when_some(folded.clone(), |button, (_, tone)| {
                    button.child(super::shell::notice_badge(
                        tone,
                        "topbar.batch.active",
                        metrics,
                    ))
                })
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if folded.is_some() && this.batch.show_active_run() {
                        cx.notify();
                    } else {
                        this.open_batch_library(cx);
                    }
                }));
            controls.push(entry);
        }
        let Some((label, tone)) = notice else {
            return controls;
        };
        let back = cx.listener(|this, _event: &gpui::ClickEvent, _window, cx| {
            if this.batch.show_active_run() {
                cx.notify();
            }
        });
        match form {
            NoticeForm::Folded if home => {}
            NoticeForm::Folded | NoticeForm::Icon => controls.push(
                super::shell::notice_icon(
                    "topbar.batch.active",
                    Icon::Play,
                    tone,
                    format!("{label} · {}", ux.batch_run_background_hint),
                    metrics,
                )
                .on_click(back),
            ),
            NoticeForm::Words => controls.push(
                super::shell::notice_words("topbar.batch.active", label, tone, metrics)
                    .tooltip(components::tooltip(SharedString::from(
                        ux.batch_run_background_hint,
                    )))
                    .on_click(back),
            ),
        }
        controls
    }

    /// What the top row says while this window drives a run: the run's state in words
    /// and its tone (R11, R13). None once the run ended.
    pub(super) fn batch_notice(&self) -> Option<(String, Tone)> {
        let ux = copy::ux();
        let page = self.batch.run()?;
        if page.is_finished() {
            return None;
        }
        let done = page.finished_hosts().to_string();
        let total = page.total_hosts().to_string();
        let label = if page.failed_hosts() > 0 {
            copy::fill(
                ux.batch_indicator_failed,
                &[("failed", &page.failed_hosts().to_string())],
            )
        } else {
            copy::fill(
                ux.batch_indicator_running,
                &[("done", &done), ("total", &total)],
            )
        };
        Some((label, run_state_tone(page.state())))
    }

    // -----------------------------------------------------------------------
    // The dialog
    // -----------------------------------------------------------------------

    /// The batch dialog, when one is open.
    pub(super) fn batch_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        use crate::app::batch::BatchCenterView;
        match self.batch.view() {
            BatchCenterView::Closed => None,
            BatchCenterView::Editor => Some(self.batch_preview_dialog(window, cx)),
            BatchCenterView::ActiveRun => Some(self.batch_run_dialog(window, cx)),
            BatchCenterView::Library => Some(self.batch_library_dialog(window, cx)),
            BatchCenterView::History => Some(self.batch_history_dialog(window, cx)),
        }
    }

    /// The switch between the four surfaces, drawn at the top of each of them.
    ///
    /// The run and the definition being edited are offered only when they exist: a
    /// switch to a page that has nothing to show would be a dead end. Everything here is
    /// a pointer target the size of a control and a keyboard stop in the lists, so the
    /// centre is reachable without a mouse (R4).
    fn batch_surface_switch(&self, cx: &mut Context<Self>) -> Div {
        use crate::app::batch::BatchCenterView;
        let ux = copy::ux();
        let metrics = self.metrics();
        let current = self.batch.view();
        let mut segments = components::segmented(metrics);
        let entries = [
            (
                BatchCenterView::Library,
                ux.batch_switch_library,
                "batch.switch.library",
                true,
            ),
            (
                BatchCenterView::Editor,
                ux.batch_switch_editor,
                "batch.switch.editor",
                self.batch.concurrency().is_some(),
            ),
            (
                BatchCenterView::ActiveRun,
                ux.batch_switch_run,
                "batch.switch.run",
                self.batch.has_active_run(),
            ),
            (
                BatchCenterView::History,
                ux.batch_switch_history,
                "batch.switch.history",
                true,
            ),
        ];
        for (surface, label, id, available) in entries {
            if !available {
                continue;
            }
            let selected = current == surface;
            segments = segments.child(
                components::segment(label, selected, metrics)
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .when(!selected, |segment| {
                        segment.cursor_pointer().on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.show_batch_surface(surface, cx);
                            },
                        ))
                    }),
            );
        }
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.switch".to_owned())
            .child(segments)
    }

    /// The saved definitions: what is written down, what can be edited, and what goes.
    fn batch_library_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Layers),
            ux.batch_library_title.to_owned(),
            Some(ux.batch_library_subtitle.to_owned()),
            "batch.library.close",
            cx,
            |this: &mut Self, cx: &mut Context<Self>| this.dismiss_batch(cx),
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
            .child(self.batch_surface_switch(cx));
        if let Some(problem) = self.batch.problem() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "batch.library.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.to_owned())),
            );
        }
        if let Some(problem) = self.batch.library().problem() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "batch.library.file_problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.to_owned())),
            );
        }
        let tasks = self.batch.library().tasks();
        if tasks.is_empty() {
            body = body.child(
                components::state_card(
                    Tone::Accent,
                    ux.batch_library_empty_title,
                    ux.batch_library_empty_body,
                    None,
                    metrics,
                )
                .debug_selector(|| "batch.library.empty".to_owned()),
            );
        }
        let mut rows = div()
            .id("batch.library.rows")
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .max_h(metrics.scale(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .debug_selector(|| "batch.library.rows".to_owned());
        for (index, task) in tasks.iter().take(MAX_LIBRARY_ROWS).enumerate() {
            rows = rows.child(self.batch_library_row(index, task, cx));
        }
        body = body.child(rows);
        let hidden = tasks.len().saturating_sub(MAX_LIBRARY_ROWS);
        if hidden > 0 {
            body = body.child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.library.more".to_owned())
                    .child(library_more_line(ux, hidden)),
            );
        }
        let footer = components::modal_footer_noted(
            div()
                .debug_selector(|| "batch.library.note".to_owned())
                .child(ux.batch_no_rollback),
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .flex_none()
                .gap(metrics.scale(spacing::TIGHT))
                // The primary action is the last one, at the right edge (ADR-7).
                .child(
                    components::compact_button(ux.batch_close, Variant::Ghost, metrics)
                        .id("batch.library.dismiss")
                        .debug_selector(|| "batch.library.dismiss".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.dismiss_batch(cx);
                        })),
                )
                .child(
                    components::compact_button(ux.batch_library_new, Variant::Primary, metrics)
                        .id("batch.library.new")
                        .debug_selector(|| "batch.library.new".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.open_batch_preview(cx);
                        })),
                ),
            metrics,
        );
        self.modal(
            window,
            "batch.library",
            BATCH_DIALOG_WIDTH,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// One saved definition: what it would run on, and the three things to do with it.
    fn batch_library_row(
        &self,
        index: usize,
        task: &shellune_core::BatchTask,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ux = copy::ux();
        let metrics = self.metrics();
        let id = task.id();
        let selected = self.batch.library_selected() == index;
        let name = task.name().unwrap_or(ux.batch_untitled).to_owned();
        let meta = copy::fill(
            ux.batch_library_row_meta,
            &[
                ("hosts", &task.targets().len().to_string()),
                ("steps", &task.steps().len().to_string()),
                ("concurrency", &task.concurrency().to_string()),
            ],
        );
        let asking = self.batch.delete_asked() == Some(id);
        let mut row = components::list_row_frame(
            if selected {
                components::ControlState::Selected
            } else {
                components::ControlState::Resting
            },
            metrics,
        )
        .id(SharedString::from(format!("batch.library.row.{index}")))
        .debug_selector(move || format!("batch.library.row.{index}"))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(metrics.scale(spacing::HAIRLINE))
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.label_size())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(SharedString::from(name)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(SharedString::from(meta)),
                ),
        );
        if asking {
            row = row
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::DANGER))
                        .debug_selector(move || format!("batch.library.delete_ask.{index}"))
                        .child(ux.batch_library_delete_ask),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            components::compact_button(
                                ux.batch_library_delete_no,
                                Variant::Ghost,
                                metrics,
                            )
                            .id(SharedString::from(format!(
                                "batch.library.delete_no.{index}"
                            )))
                            .debug_selector(move || format!("batch.library.delete_no.{index}"))
                            .on_click(cx.listener(
                                |this, _event, _window, cx| {
                                    this.batch.cancel_delete_saved();
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            components::compact_button(
                                ux.batch_library_delete_yes,
                                Variant::Danger,
                                metrics,
                            )
                            .id(SharedString::from(format!(
                                "batch.library.delete_yes.{index}"
                            )))
                            .debug_selector(move || format!("batch.library.delete_yes.{index}"))
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    this.delete_saved_batch(id, cx);
                                },
                            )),
                        ),
                );
        } else {
            row = row.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::compact_button(
                            ux.batch_library_edit,
                            Variant::Secondary,
                            metrics,
                        )
                        .id(SharedString::from(format!("batch.library.edit.{index}")))
                        .debug_selector(move || format!("batch.library.edit.{index}"))
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.edit_saved_batch(id, cx);
                            },
                        )),
                    )
                    .child(
                        components::compact_button(
                            ux.batch_library_delete,
                            Variant::Ghost,
                            metrics,
                        )
                        .id(SharedString::from(format!("batch.library.delete.{index}")))
                        .debug_selector(move || format!("batch.library.delete.{index}"))
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.batch.ask_delete_saved(id);
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        row
    }

    /// The stored runs: a list to scan, and the detail of the one that was opened.
    fn batch_history_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let open = self
            .batch
            .stored_open()
            .and_then(|id| self.batch.history().entry(id));
        let (title, subtitle) = match open {
            Some(_) => (ux.batch_history_title.to_owned(), None),
            None => (
                ux.batch_history_title.to_owned(),
                Some(ux.batch_history_subtitle.to_owned()),
            ),
        };
        let (header, subtitle) = self.modal_header(
            Some(Icon::Layers),
            title,
            subtitle,
            "batch.history.close",
            cx,
            |this: &mut Self, cx: &mut Context<Self>| this.dismiss_batch(cx),
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
            .child(self.batch_surface_switch(cx));
        if let Some(problem) = self.batch.problem() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "batch.history.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.to_owned())),
            );
        }
        if let Some(problem) = self.batch.history().problem() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "batch.history.file_problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.to_owned())),
            );
        }
        body = body.child(match self.batch.stored_open() {
            Some(id) if self.batch.history().entry(id).is_some() => {
                self.batch_history_detail(id, cx)
            }
            _ => self.batch_history_list(cx),
        });
        let footer = components::modal_footer_noted(
            div()
                .debug_selector(|| "batch.history.note".to_owned())
                .child(ux.batch_no_rollback),
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .flex_none()
                .gap(metrics.scale(spacing::TIGHT))
                .children(self.batch.stored_open().map(|_| {
                    components::compact_button(
                        ux.batch_diagnostics_copy,
                        Variant::Secondary,
                        metrics,
                    )
                    .id("batch.history.diagnostics")
                    .debug_selector(|| "batch.history.diagnostics".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        let Some(run) = this.batch.stored_summary().cloned() else {
                            return;
                        };
                        this.copy_batch_diagnostics(&run, cx);
                    }))
                }))
                .child(
                    components::compact_button(ux.batch_close, Variant::Ghost, metrics)
                        .id("batch.history.dismiss")
                        .debug_selector(|| "batch.history.dismiss".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.dismiss_batch(cx);
                        })),
                ),
            metrics,
        );
        self.modal(
            window,
            "batch.history",
            BATCH_DIALOG_WIDTH,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// The stored runs the filter keeps, newest first.
    fn batch_history_list(&self, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let mut filters = components::segmented(metrics);
        let any_selected = self.batch.history_filter().is_none();
        filters = filters.child(
            components::segment(ux.batch_history_filter_any, any_selected, metrics)
                .id("batch.history.filter.any")
                .debug_selector(|| "batch.history.filter.any".to_owned())
                .when(!any_selected, |segment| {
                    segment
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.batch.set_history_filter(None);
                            cx.notify();
                        }))
                }),
        );
        for state in HISTORY_FILTERS {
            let selected = self.batch.history_filter() == Some(state);
            let id = history_filter_selector(state);
            filters = filters.child(
                components::segment(run_state_text(state, ux), selected, metrics)
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .when(!selected, |segment| {
                        segment.cursor_pointer().on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.batch.set_history_filter(Some(state));
                                cx.notify();
                            },
                        ))
                    }),
            );
        }
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::section_label(ux.batch_history_filter, metrics))
                    .child(filters),
            );
        let entries = self.batch.history_visible();
        if entries.is_empty() {
            column = column.child(
                components::state_card(
                    Tone::Neutral,
                    ux.batch_history_empty,
                    ux.batch_history_subtitle,
                    None,
                    metrics,
                )
                .debug_selector(|| "batch.history.empty".to_owned()),
            );
        }
        let mut rows = div()
            .id("batch.history.rows")
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .max_h(metrics.scale(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .debug_selector(|| "batch.history.rows".to_owned());
        for (index, entry) in entries.iter().take(MAX_HISTORY_ROWS).enumerate() {
            rows = rows.child(self.batch_history_row(index, entry.id(), entry, cx));
        }
        column = column.child(rows);
        let hidden =
            entries.len().saturating_sub(MAX_HISTORY_ROWS) + self.batch.history().omitted();
        if hidden > 0 {
            column = column.child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.history.more".to_owned())
                    .child(copy::fill(
                        ux.batch_history_omitted,
                        &[("count", &hidden.to_string())],
                    )),
            );
        }
        column
    }

    /// One stored run: when it started, what it ended as, and how to read it.
    fn batch_history_row(
        &self,
        index: usize,
        id: shellune_core::BatchRunId,
        entry: &crate::app::batch::HistoryEntry,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ux = copy::ux();
        let metrics = self.metrics();
        let selected = self.batch.history_selected() == index;
        let mut row = components::list_row_frame(
            if selected {
                components::ControlState::Selected
            } else {
                components::ControlState::Resting
            },
            metrics,
        )
        .id(SharedString::from(format!("batch.history.row.{index}")))
        .debug_selector(move || format!("batch.history.row.{index}"));
        let Some(run) = entry.summary() else {
            return row.child(
                div()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(id.to_string())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::DANGER))
                            .debug_selector(move || format!("batch.history.damaged.{index}"))
                            .child(entry.problem().map_or_else(
                                || ux.batch_history_damaged.to_owned(),
                                str::to_owned,
                            )),
                    ),
            );
        };
        let name = run
            .task()
            .name()
            .map_or_else(|| ux.batch_untitled.to_owned(), str::to_owned);
        let when = copy::fill(
            ux.batch_history_when,
            &[(
                "time",
                &copy::relative_time(
                    run.started_at_unix(),
                    crate::app::remote::now_unix(),
                    self.workspace.locale(),
                ),
            )],
        );
        row = row
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{when} · {}",
                                copy::fill(
                                    ux.batch_history_hosts,
                                    &[
                                        ("done", &run.finished_hosts().to_string()),
                                        ("total", &run.hosts().len().to_string()),
                                    ],
                                )
                            ))),
                    ),
            )
            .child(components::chip(
                run_state_text(run.state(), ux),
                run_state_tone(run.state()),
                metrics,
            ))
            .child(
                components::compact_button(ux.batch_history_open, Variant::Secondary, metrics)
                    .id(SharedString::from(format!("batch.history.open.{index}")))
                    .debug_selector(move || format!("batch.history.open.{index}"))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.open_stored_batch(id, cx);
                    })),
            );
        row
    }

    /// One stored run in full: its hosts, their steps, the exit statuses, the timeline
    /// and the output of the host that is chosen.
    fn batch_history_detail(&self, id: shellune_core::BatchRunId, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let entry = self.batch.history().entry(id);
        let Some(run) = entry.and_then(crate::app::batch::HistoryEntry::summary) else {
            return self.batch_history_list(cx);
        };
        let name = run
            .task()
            .name()
            .map_or_else(|| ux.batch_untitled.to_owned(), str::to_owned);
        let when = copy::fill(
            ux.batch_history_when,
            &[(
                "time",
                &copy::relative_time(
                    run.started_at_unix(),
                    crate::app::remote::now_unix(),
                    self.workspace.locale(),
                ),
            )],
        );
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.history.detail".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(name)),
                    )
                    .child(components::chip(
                        run_state_text(run.state(), ux),
                        run_state_tone(run.state()),
                        metrics,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "batch.history.detail.when".to_owned())
                            .child(SharedString::from(when)),
                    )
                    .child(components::chip(
                        copy::fill(
                            ux.batch_count_steps,
                            &[("count", &run.task().steps().len().to_string())],
                        ),
                        Tone::Neutral,
                        metrics,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::compact_button(ux.batch_history_back, Variant::Ghost, metrics)
                            .id("batch.history.back")
                            .debug_selector(|| "batch.history.back".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.batch.close_history_entry();
                                cx.notify();
                            })),
                    )
                    .child(
                        components::compact_button(ux.batch_run_retry, Variant::Secondary, metrics)
                            .id("batch.history.retry")
                            .debug_selector(|| "batch.history.retry".to_owned())
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                // The retry is a new definition and goes through the
                                // editor and its confirmation; nothing starts here (R22).
                                match this.batch.retry_saved(cx, id) {
                                    Ok(()) => {}
                                    Err(problem) => this.batch.set_problem(problem),
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                components::banner(Tone::Neutral, metrics)
                    .debug_selector(|| "batch.history.retry_note".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(ux.batch_retry_confirm),
            );
        let selected = self.batch.stored_host();
        let mut hosts = div()
            .id("batch.history.hosts")
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .max_h(metrics.scale(HOSTS_MAX_HEIGHT))
            .overflow_y_scroll()
            .debug_selector(|| "batch.history.hosts".to_owned());
        for (index, host) in run.hosts().iter().take(MAX_HISTORY_HOSTS).enumerate() {
            let host_id = host.target().host_id();
            let mut steps = div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(move || format!("batch.history.steps.{index}"));
            for step in host.steps() {
                let exit = step
                    .exit()
                    .map(|code| copy::fill(ux.batch_exit, &[("code", &code.to_string())]))
                    .unwrap_or_default();
                let label = if exit.is_empty() {
                    step_state_text(step.state(), ux).to_owned()
                } else {
                    format!("{} {exit}", step_state_text(step.state(), ux))
                };
                steps = steps.child(components::chip(
                    label,
                    step_state_tone(step.state()),
                    metrics,
                ));
            }
            let mut row = components::list_row_frame(
                if selected == Some(host_id) {
                    components::ControlState::Selected
                } else {
                    components::ControlState::Resting
                },
                metrics,
            )
            .flex_col()
            .items_start()
            .gap(metrics.scale(spacing::HAIRLINE))
            .id(SharedString::from(format!("batch.history.host.{index}")))
            .cursor_pointer()
            .debug_selector(move || format!("batch.history.host.{index}"))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.select_stored_host(host_id, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(host.target().label().to_owned())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(host.target().destination())),
                    )
                    .child(components::chip(
                        host_state_text(host.state(), ux),
                        host_state_tone(host.state()),
                        metrics,
                    )),
            )
            .child(steps);
            if let Some(reason) = host.detail() {
                row = row.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(redact_batch_text(reason))),
                );
            }
            hosts = hosts.child(row);
        }
        let hidden = run.hosts().len().saturating_sub(MAX_HISTORY_HOSTS);
        if hidden > 0 {
            hosts = hosts.child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.history.hosts.more".to_owned())
                    .child(copy::fill(
                        ux.batch_targets_more,
                        &[("count", &hidden.to_string())],
                    )),
            );
        }
        column = column.child(hosts);
        column = column.child(self.batch_history_output(run, cx));
        column
    }

    /// The output and the step times of the chosen host of a stored run.
    fn batch_history_output(&self, run: &shellune_core::BatchRun, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let Some(host) = self.batch.stored_host() else {
            return div()
                .flex_none()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .debug_selector(|| "batch.history.output.no_host".to_owned())
                .child(ux.batch_history_no_host);
        };
        let detail = self.batch.stored_detail_open();
        let copy_text = detail
            .map(|detail| {
                detail
                    .output
                    .lines()
                    .iter()
                    .map(|line| redact_batch_text(&line.text))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty());
        let mut output = div()
            .id("batch.history.output")
            .flex()
            .flex_col()
            .h(metrics.scale(OUTPUT_HEIGHT))
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HAIRLINE))
            .p(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SEGMENT_TRACK))
            .font_family(crate::app::terminal::monospace_family())
            .debug_selector(|| "batch.history.output".to_owned());
        match detail.map(|detail| detail.output.lines()) {
            Some(lines) if !lines.is_empty() => {
                if let Some(dropped) = detail.map(|detail| detail.output.dropped())
                    && dropped > 0
                {
                    output = output.child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "batch.history.output.dropped".to_owned())
                            .child(copy::fill(
                                ux.batch_run_output_dropped,
                                &[("count", &dropped.to_string())],
                            )),
                    );
                }
                for line in lines {
                    let text = if line.text.is_empty() {
                        " ".to_owned()
                    } else {
                        line.text.clone()
                    };
                    output = output.child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(if line.is_error() {
                                palette::DANGER
                            } else {
                                palette::TEXT_SECONDARY
                            }))
                            .child(SharedString::from(redact_batch_text(&text))),
                    );
                }
            }
            // A log that was not read is not a log that printed nothing: the empty state
            // says "no output", so a read that failed has to say so instead of borrowing
            // it, or a damaged or missing run log would read as a quiet host.
            None => {
                let text = match self.batch.problem() {
                    Some(problem) => copy::fill(
                        ux.batch_history_output_unread_reason,
                        &[("reason", &redact_batch_text(problem))],
                    ),
                    None => ux.batch_history_output_unread.to_owned(),
                };
                output = output.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "batch.history.output.unread".to_owned())
                        .child(SharedString::from(text)),
                );
            }
            Some(_) => {
                output = output.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "batch.history.output.empty".to_owned())
                        .child(ux.batch_run_output_empty),
                );
            }
        }
        // The times come from the log, so the offsets are the run's own and not the
        // wall clock the page was opened at.
        let mut timeline = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "batch.history.timeline".to_owned())
            .child(components::section_label(ux.batch_run_timeline, metrics));
        let start = run.started_at_unix();
        if let Some(record) = run.host(host) {
            for (index, step) in record.steps().iter().enumerate() {
                let command = run
                    .task()
                    .steps()
                    .get(index)
                    .map(|step| step.command().to_owned())
                    .unwrap_or_default();
                let offsets = detail
                    .and_then(|detail| detail.time_of(step.index()))
                    .map(|time| match (time.started, time.finished) {
                        (Some(started), Some(finished)) => format!(
                            "{} → {}",
                            offset_text(start, started),
                            offset_text(start, finished)
                        ),
                        (Some(started), None) => offset_text(start, started),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                timeline = timeline.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(metrics.scale(spacing::TIGHT))
                        .debug_selector(move || format!("batch.history.timeline.{index}"))
                        .child(
                            div()
                                .flex_none()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(copy::fill(
                                    ux.batch_run_step,
                                    &[("index", &(index + 1).to_string())],
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                .child(SharedString::from(command)),
                        )
                        .child(components::chip(
                            step_state_text(step.state(), ux),
                            step_state_tone(step.state()),
                            metrics,
                        ))
                        .when(!offsets.is_empty(), |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED))
                                    .child(SharedString::from(offsets)),
                            )
                        }),
                );
            }
        }
        let mut heading = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .child(components::section_label(ux.batch_run_output, metrics));
        if let Some(copy_text) = copy_text {
            heading = heading.child(
                self.icon_text_button(
                    SharedString::from("batch.history.output.copy"),
                    Icon::Copy,
                    ux.batch_output_copy,
                    Variant::Ghost,
                    cx,
                    move |this, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                        this.toast(Tone::Positive, copy::ux().batch_output_copied, None, cx);
                    },
                )
                .h(metrics.compact_control_height()),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .child(heading)
            .child(output)
            .child(timeline)
    }

    fn batch_preview_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Layers),
            ux.batch_title.to_owned(),
            Some(ux.batch_preview_subtitle.to_owned()),
            "batch.dialog.close",
            cx,
            |this: &mut Self, cx: &mut Context<Self>| this.close_batch(cx),
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
            .child(self.batch_surface_switch(cx))
            .child(self.batch_targets_section(cx));
        if let Some(note) = self.batch.note() {
            body = body
                .child(
                    components::banner(Tone::Accent, metrics)
                        .debug_selector(|| "batch.note".to_owned())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(SharedString::from(note.to_owned())),
                )
                // A retry says what it will and will not do before it is confirmed, in
                // the same words the run page and the history use (R22).
                .child(
                    components::banner(Tone::Neutral, metrics)
                        .debug_selector(|| "batch.retry.confirm_note".to_owned())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(ux.batch_retry_confirm),
                );
        }
        body = body
            .child(self.batch_steps_section(cx))
            .child(self.batch_limits_section(cx));
        if let Some(problem) = self.batch.problem() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "batch.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.to_owned())),
            );
        }
        let footer = components::modal_footer_noted(
            div()
                .debug_selector(|| "batch.footer.note".to_owned())
                .child(ux.batch_no_rollback),
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(metrics.scale(spacing::TIGHT))
                .child(
                    // Saving is the other half of the editor: it writes the
                    // definition and starts nothing, which is what makes a task
                    // reusable after a restart (R12).
                    components::compact_button(ux.batch_save_draft, Variant::Secondary, metrics)
                        .id("batch.preview.save")
                        .debug_selector(|| "batch.preview.save".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.save_batch_draft(cx);
                        })),
                )
                .child(
                    components::compact_button(ux.batch_cancel, Variant::Ghost, metrics)
                        .id("batch.preview.cancel")
                        .debug_selector(|| "batch.preview.cancel".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.close_batch(cx);
                        })),
                )
                .child(if self.batch.second_run_refusal().is_some() {
                    // A second run would replace the page of the one already going,
                    // so the button is drawn unable to answer instead of answering
                    // with a refusal after the click (R13).
                    components::button_with_state(
                        ux.batch_confirm,
                        Variant::Primary,
                        components::ControlState::Disabled,
                        metrics,
                    )
                    .id("batch.preview.confirm")
                    .debug_selector(|| "batch.preview.confirm".to_owned())
                    .tooltip(components::tooltip(SharedString::from(
                        ux.batch_run_in_progress_refusal,
                    )))
                } else {
                    components::button(ux.batch_confirm, Variant::Primary, metrics)
                        .id("batch.preview.confirm")
                        .debug_selector(|| "batch.preview.confirm".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.confirm_batch(cx);
                        }))
                }),
            metrics,
        );
        self.modal(
            window,
            "batch.dialog",
            BATCH_DIALOG_WIDTH,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// The targets: the scope they came from, how many there are, and the rows.
    fn batch_targets_section(&self, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let count = self.batch.target_count();
        let mut scopes = components::segmented(metrics);
        for scope in SelectionScope::ALL {
            let selected = self.batch.scope() == scope;
            scopes = scopes.child(
                components::segment(scope.label(ux), selected, metrics)
                    .id(scope.debug_selector())
                    .debug_selector(move || format!("batch.scope.{}", scope.debug_selector()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.select_batch_scope(scope, cx);
                    })),
            );
        }
        let count_text = copy::fill(ux.batch_targets_count, &[("count", &count.to_string())]);
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.targets".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::section_label(ux.batch_targets_section, metrics))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(ux.batch_scope_label),
                    )
                    .child(scopes),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.label_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "batch.targets.count".to_owned())
                    .child(SharedString::from(count_text)),
            );
        if count == 0 {
            column = column.child(
                components::banner(Tone::Warning, metrics)
                    .debug_selector(|| "batch.targets.empty".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(ux.batch_no_targets),
            );
        }
        for (label, reason) in self.batch.refused() {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.targets.refused".to_owned())
                    .child(components::chip(
                        ux.batch_targets_refused,
                        Tone::Warning,
                        metrics,
                    ))
                    .child(SharedString::from(format!("{label} · {reason}"))),
            );
        }
        let mut rows = div()
            .id("batch.targets.rows")
            .flex()
            .flex_col()
            .max_h(metrics.scale(TARGETS_MAX_HEIGHT))
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "batch.targets.rows".to_owned());
        for (index, target) in self
            .batch
            .targets()
            .iter()
            .take(MAX_PREVIEW_ROWS)
            .enumerate()
        {
            rows = rows.child(batch_target_row(target, index, metrics));
        }
        column = column.child(rows);
        let hidden = count.saturating_sub(MAX_PREVIEW_ROWS);
        if hidden > 0 {
            column = column.child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.targets.more".to_owned())
                    .child(copy::fill(
                        ux.batch_targets_more,
                        &[("count", &hidden.to_string())],
                    )),
            );
        }
        column
    }

    /// The commands: one field per step, with add and remove.
    fn batch_steps_section(&self, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let count = self.batch.steps().len();
        let label = copy::fill(ux.batch_count_steps, &[("count", &count.to_string())]);
        let mut fields = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.steps".to_owned());
        for (index, field) in self.batch.steps().iter().enumerate() {
            let number = format!("{}", index + 1);
            fields = fields.child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .debug_selector(move || format!("batch.step.{index}"))
                    .child(
                        div()
                            .flex_none()
                            .w(metrics.scale(STEP_NUMBER_WIDTH))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(number)),
                    )
                    .child(
                        // The text field draws its own border; a frame around it drew a
                        // second one.
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .debug_selector(move || format!("batch.step.field.{index}"))
                            .child(field.clone()),
                    )
                    .child(
                        // The order is the order they run in, so it is moved here
                        // rather than retyped: two steps swap, and the first and the
                        // last say they have nowhere to go (R12).
                        components::icon_button("↑", metrics)
                            .id(SharedString::from(format!("batch.step.up.{index}")))
                            .debug_selector(move || format!("batch.step.up.{index}"))
                            .tooltip(components::tooltip(SharedString::from(ux.batch_step_up)))
                            .when(index == 0, |button| button.opacity(0.38))
                            .when(index > 0, |button| {
                                button.cursor_pointer().on_click(cx.listener(
                                    move |this, _event, _window, cx| {
                                        let _ = this.batch.move_step_up(index);
                                        cx.notify();
                                    },
                                ))
                            }),
                    )
                    .child(
                        components::icon_button("↓", metrics)
                            .id(SharedString::from(format!("batch.step.down.{index}")))
                            .debug_selector(move || format!("batch.step.down.{index}"))
                            .tooltip(components::tooltip(SharedString::from(ux.batch_step_down)))
                            .when(index + 1 >= count, |button| button.opacity(0.38))
                            .when(index + 1 < count, |button| {
                                button.cursor_pointer().on_click(cx.listener(
                                    move |this, _event, _window, cx| {
                                        let _ = this.batch.move_step_down(index);
                                        cx.notify();
                                    },
                                ))
                            }),
                    )
                    .child(
                        components::button_with_state(
                            ux.batch_step_remove,
                            Variant::Ghost,
                            if count > 1 {
                                components::ControlState::Resting
                            } else {
                                components::ControlState::Disabled
                            },
                            metrics,
                        )
                        .h(metrics.compact_control_height())
                        .px(metrics.scale(spacing::SNUG))
                        .text_size(metrics.caption_size())
                        .id(SharedString::from(format!("batch.step.remove.{index}")))
                        .debug_selector(move || format!("batch.step.remove.{index}"))
                        .when(count > 1, |button| {
                            button.on_click(cx.listener(move |this, _event, _window, cx| {
                                let _ = this.batch.remove_step(index);
                                cx.notify();
                            }))
                        }),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::section_label(ux.batch_steps_section, metrics))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "batch.steps.count".to_owned())
                            .child(SharedString::from(label)),
                    )
                    .child(
                        components::compact_button(ux.batch_step_add, Variant::Secondary, metrics)
                            .id("batch.step.add")
                            .debug_selector(|| "batch.step.add".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                let _ = this.batch.add_step(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(fields)
    }

    /// The concurrency, the failure policy and the optional name.
    fn batch_limits_section(&self, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let mut policies = components::segmented(metrics);
        for policy in FailurePolicy::ALL {
            let selected = self.batch.on_failure() == policy;
            let label = match policy {
                FailurePolicy::SkipHost => ux.batch_failure_skip,
                FailurePolicy::StopAfterFailure => ux.batch_failure_stop,
            };
            policies = policies.child(
                components::segment(label, selected, metrics)
                    .id(batch_policy_selector(policy))
                    .debug_selector(move || batch_policy_selector(policy).to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.batch.set_on_failure(policy);
                        cx.notify();
                    })),
            );
        }
        let concurrency = self.batch.concurrency().cloned();
        let name = self.batch.name().cloned();
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.limits".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::section_label(ux.batch_failure, metrics))
                    .child(policies),
            )
            // The two numbers of the run sit side by side as one row of the form grid
            // and stack in a narrow dialog (ADR-7).
            .child(
                components::form_row(metrics)
                    .child(components::form_field(
                        ux.batch_concurrency,
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(metrics.scale(spacing::TIGHT))
                            .children(concurrency.map(|field| {
                                div()
                                    .flex()
                                    .w(metrics.scale(CONCURRENCY_WIDTH))
                                    .debug_selector(|| "batch.concurrency".to_owned())
                                    .child(field)
                            }))
                            .child(components::form_hint(
                                copy::fill(
                                    ux.batch_concurrency_hint,
                                    &[("max", &shellune_core::MAX_BATCH_CONCURRENCY.to_string())],
                                ),
                                metrics,
                            )),
                        0.4,
                        components::FORM_FIELD_MIN_WIDTH,
                        metrics,
                    ))
                    .child(components::form_field(
                        ux.batch_name,
                        div().flex().w_full().children(name.map(|field| {
                            div()
                                .flex()
                                .flex_1()
                                .min_w(metrics.scale(NAME_MIN_WIDTH))
                                .debug_selector(|| "batch.name".to_owned())
                                .child(field)
                        })),
                        0.6,
                        components::FORM_FIELD_MIN_WIDTH,
                        metrics,
                    )),
            )
    }

    // -----------------------------------------------------------------------
    // The run
    // -----------------------------------------------------------------------

    fn batch_run_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let Some(page) = self.batch.run() else {
            return div();
        };
        let title = page
            .confirmed()
            .task()
            .name()
            .map_or_else(|| ux.batch_title.to_owned(), str::to_owned);
        let state_text = run_state_text(page.state(), ux);
        let (header, subtitle) = self.modal_header(
            Some(Icon::Layers),
            title,
            Some(state_text.to_owned()),
            "batch.run.close",
            cx,
            |this: &mut Self, cx: &mut Context<Self>| this.dismiss_batch(cx),
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
            .child(self.batch_surface_switch(cx))
            .child(self.batch_run_status(page));
        if let Some(problem) = page.problem() {
            body = body.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "batch.run.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(problem.to_owned())),
            );
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_start()
                .gap(metrics.scale(spacing::SNUG))
                .child(self.batch_run_hosts(page, cx))
                .child(self.batch_run_side(page, cx)),
        );
        let footer = components::modal_footer_noted(
            div()
                .debug_selector(|| "batch.run.note".to_owned())
                .child(ux.batch_no_rollback),
            self.batch_run_controls(page, cx),
            metrics,
        );
        self.modal(
            window,
            "batch.run",
            BATCH_DIALOG_WIDTH,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// The state line, the progress and the explanation of a state that is not obvious.
    fn batch_run_status(&self, page: &BatchRunPage) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let tone = run_state_tone(page.state());
        let text = copy::fill(
            ux.batch_run_progress,
            &[
                ("done", &page.finished_hosts().to_string()),
                ("total", &page.total_hosts().to_string()),
                ("running", &page.running_hosts().to_string()),
                (
                    "concurrency",
                    &page.confirmed().task().concurrency().to_string(),
                ),
            ],
        );
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.run.status".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::chip(
                        run_state_text(page.state(), ux),
                        tone,
                        metrics,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "batch.run.progress".to_owned())
                            .child(SharedString::from(text)),
                    )
                    .child(components::chip(
                        copy::fill(
                            ux.batch_count_steps,
                            &[("count", &page.confirmed().task().steps().len().to_string())],
                        ),
                        Tone::Neutral,
                        metrics,
                    ))
                    .when(page.failed_hosts() > 0, |row| {
                        row.child(components::chip(
                            format!("{} {}", page.failed_hosts(), ux.batch_host_failed),
                            Tone::Danger,
                            metrics,
                        ))
                    }),
            );
        // The state's own sentence, where it changes what the user expects: a pause is
        // not a stop, and a cancel is a request until the remote command has ended.
        let explanation = match page.state() {
            RunState::Paused => Some(ux.batch_run_paused),
            RunState::Cancelling => Some(ux.batch_run_cancelling),
            _ => None,
        };
        if let Some(explanation) = explanation {
            column = column.child(
                components::banner(tone, metrics)
                    .debug_selector(|| "batch.run.explanation".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(explanation),
            );
        }
        column
    }

    /// Pause, resume, cancel, retry and close, in the state the run allows.
    fn batch_run_controls(&self, page: &BatchRunPage, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let finished = page.is_finished();
        let mut controls = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.run.controls".to_owned());
        if !finished {
            if page.state() == RunState::Paused {
                controls = controls.child(
                    components::compact_button(ux.batch_run_resume, Variant::Primary, metrics)
                        .id("batch.run.resume")
                        .debug_selector(|| "batch.run.resume".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.resume_batch(cx);
                        })),
                );
            } else {
                controls = controls.child(
                    components::compact_button(ux.batch_run_pause, Variant::Secondary, metrics)
                        .id("batch.run.pause")
                        .debug_selector(|| "batch.run.pause".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.pause_batch(cx);
                        })),
                );
            }
            let cancelling = page.state() == RunState::Cancelling;
            controls = controls.child(
                components::compact_button(
                    if cancelling {
                        run_state_text(RunState::Cancelling, ux)
                    } else {
                        ux.batch_run_cancel
                    },
                    Variant::Ghost,
                    metrics,
                )
                .id("batch.run.cancel")
                .debug_selector(|| "batch.run.cancel".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.cancel_batch(cx);
                })),
            );
        }
        if page.can_retry() {
            controls = controls.child(
                components::compact_button(ux.batch_run_retry, Variant::Secondary, metrics)
                    .id("batch.run.retry")
                    .debug_selector(|| "batch.run.retry".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.retry_batch(cx);
                    })),
            );
        }
        // The report of the run can leave the application as text, with the
        // credentials taken out, whenever the run has said something (R13).
        controls = controls.child(
            components::compact_button(ux.batch_diagnostics_copy, Variant::Ghost, metrics)
                .id("batch.run.diagnostics")
                .debug_selector(|| "batch.run.diagnostics".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    let Some(run) = this.batch.run().map(|page| page.run().clone()) else {
                        return;
                    };
                    this.copy_batch_diagnostics(&run, cx);
                })),
        );
        if finished {
            controls = controls.child(
                components::compact_button(ux.batch_close, Variant::Primary, metrics)
                    .id("batch.run.close.action")
                    .debug_selector(|| "batch.run.close.action".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.close_batch(cx);
                    })),
            );
        } else {
            // Leaving is not stopping: the runner keeps its page and the top bar
            // indicator brings this one back, with the chosen host and its output
            // still in place (R11, R13).
            controls = controls.child(
                components::compact_button(ux.batch_run_background, Variant::Primary, metrics)
                    .id("batch.run.background")
                    .debug_selector(|| "batch.run.background".to_owned())
                    .tooltip(components::tooltip(SharedString::from(
                        ux.batch_run_background_hint,
                    )))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.background_batch(cx);
                    })),
            );
        }
        controls
    }

    /// One row per host, with its steps, its exit results and its reason.
    fn batch_run_hosts(&self, page: &BatchRunPage, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let selected = page.selected();
        let active_filter = self.batch.run_host_filter();
        let mut filter_bar = None;
        if page.total_hosts() > 1 {
            let pill = |selector: &'static str,
                        label: &'static str,
                        choice: Option<BatchHostFilter>,
                        cx: &mut Context<Self>| {
                components::segment(label, active_filter == choice, metrics)
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.batch.set_run_host_filter(choice);
                        cx.notify();
                    }))
            };
            let mut pills = components::segmented(metrics)
                .debug_selector(|| "batch.run.filters".to_owned())
                .child(pill("batch.run.filter.all", ux.batch_filter_all, None, cx));
            for filter in BatchHostFilter::ALL {
                let (sel, label) = match filter {
                    BatchHostFilter::Failed => ("batch.run.filter.failed", ux.batch_filter_failed),
                    BatchHostFilter::Waiting => {
                        ("batch.run.filter.waiting", ux.batch_filter_waiting)
                    }
                    BatchHostFilter::Running => {
                        ("batch.run.filter.running", ux.batch_filter_running)
                    }
                    BatchHostFilter::Done => ("batch.run.filter.done", ux.batch_filter_done),
                };
                pills = pills.child(pill(sel, label, Some(filter), cx));
            }
            filter_bar = Some(pills);
        }
        let matching_hosts: Vec<_> = page
            .run()
            .hosts()
            .iter()
            .filter(|host| active_filter.is_none_or(|f| f.matches(host.state())))
            .collect();
        let mut list = div()
            .id("batch.run.hosts")
            .flex()
            .flex_col()
            .w_full()
            .max_h(metrics.scale(RUN_HOSTS_MAX_HEIGHT))
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "batch.run.hosts".to_owned());
        for (index, host) in matching_hosts.iter().take(MAX_RUN_ROWS).enumerate() {
            let id = host.target().host_id();
            let is_selected = selected == Some(id);
            let tone = host_state_tone(host.state());
            let mut steps = div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(move || format!("batch.run.steps.{index}"));
            for step in host.steps() {
                let exit = step
                    .exit()
                    .map(|code| copy::fill(ux.batch_exit, &[("code", &code.to_string())]))
                    .unwrap_or_default();
                let label = if exit.is_empty() {
                    step_state_text(step.state(), ux).to_owned()
                } else {
                    format!("{} {exit}", step_state_text(step.state(), ux))
                };
                steps = steps.child(components::chip(
                    label,
                    step_state_tone(step.state()),
                    metrics,
                ));
            }
            let question = page.question(id);
            let reason = host
                .detail()
                .map(str::to_owned)
                .or_else(|| question.map(|question| question.text(ux)));
            // A flat list row (ADR-7): the chosen host has the selected fill and the
            // accent bar, the others light under the pointer.
            let mut row = components::list_row_frame(
                if is_selected {
                    components::ControlState::Selected
                } else {
                    components::ControlState::Resting
                },
                metrics,
            )
            .flex_col()
            .items_start()
            .gap(metrics.scale(spacing::HAIRLINE))
            .id(SharedString::from(format!("batch.run.host.{index}")))
            .cursor_pointer()
            .debug_selector(move || format!("batch.run.host.{index}"))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.batch.select_host(id);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(host.target().label().to_owned())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(host.target().destination())),
                    )
                    .child(components::chip(
                        host_state_text(host.state(), ux),
                        tone,
                        metrics,
                    ))
                    .when(host.target().starts_at() > 0, |row| {
                        row.child(components::chip(
                            copy::fill(
                                ux.batch_from_step,
                                &[(
                                    "step",
                                    &(host.target().starts_at() as usize + 1).to_string(),
                                )],
                            ),
                            Tone::Neutral,
                            metrics,
                        ))
                    }),
            )
            .child(steps);
            if let Some(reason) = reason {
                row = row.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(reason)),
                );
            }
            if let Some(question) = question {
                // Only this host is waiting; the answer goes straight back to the
                // runner, and nothing is accepted on the user's behalf.
                row = row.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(metrics.scale(spacing::TIGHT))
                        .debug_selector(move || format!("batch.run.question.{index}"))
                        .child(
                            components::compact_button(
                                ux.batch_trust_accept,
                                Variant::Secondary,
                                metrics,
                            )
                            .id(SharedString::from(format!("batch.run.accept.{index}")))
                            .debug_selector(move || format!("batch.run.accept.{index}"))
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    this.answer_batch_trust(id, true, cx);
                                },
                            )),
                        )
                        .child(
                            components::compact_button(
                                ux.batch_trust_decline,
                                Variant::Ghost,
                                metrics,
                            )
                            .id(SharedString::from(format!("batch.run.decline.{index}")))
                            .debug_selector(move || format!("batch.run.decline.{index}"))
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| {
                                    this.answer_batch_trust(id, false, cx);
                                },
                            )),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(question.text(ux)),
                        ),
                );
            }
            list = list.child(row);
        }
        let hidden = matching_hosts.len().saturating_sub(MAX_RUN_ROWS);
        if hidden > 0 {
            list = list.child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.run.more".to_owned())
                    .child(copy::fill(
                        ux.batch_targets_more,
                        &[("count", &hidden.to_string())],
                    )),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(metrics.scale(RUN_HOSTS_MIN_WIDTH))
            .gap(metrics.scale(spacing::TIGHT))
            .children(filter_bar)
            .child(list)
    }

    /// The output of one host and the times of its steps.
    fn batch_run_side(&self, page: &BatchRunPage, cx: &mut Context<Self>) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let copy_text = page
            .selected()
            .and_then(|host| page.output(host))
            .map(|log| {
                log.lines()
                    .iter()
                    .map(|line| redact_batch_text(&line.text))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty());
        let mut heading = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .child(components::section_label(ux.batch_run_output, metrics));
        if let Some(copy_text) = copy_text {
            heading = heading.child(
                self.icon_text_button(
                    SharedString::from("batch.run.output.copy"),
                    Icon::Copy,
                    ux.batch_output_copy,
                    Variant::Ghost,
                    cx,
                    move |this, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                        this.toast(Tone::Positive, copy::ux().batch_output_copied, None, cx);
                    },
                )
                .h(metrics.compact_control_height()),
            );
        }
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(metrics.scale(RUN_SIDE_MIN_WIDTH))
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.run.side".to_owned())
            .child(heading);
        let Some(host) = page.selected() else {
            return column;
        };
        let mut output = div()
            .id("batch.run.output")
            .flex()
            .flex_col()
            .h(metrics.scale(RUN_OUTPUT_HEIGHT))
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HAIRLINE))
            .p(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SEGMENT_TRACK))
            .font_family(crate::app::terminal::monospace_family())
            .debug_selector(|| "batch.run.output".to_owned());
        match page.output(host) {
            Some(log) if !log.lines().is_empty() => {
                if log.dropped() > 0 {
                    output = output.child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "batch.run.output.dropped".to_owned())
                            .child(copy::fill(
                                ux.batch_run_output_dropped,
                                &[("count", &log.dropped().to_string())],
                            )),
                    );
                }
                for line in log.lines() {
                    let text = if line.text.is_empty() {
                        " ".to_owned()
                    } else {
                        line.text.clone()
                    };
                    output = output.child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(if line.is_error() {
                                palette::DANGER
                            } else {
                                palette::TEXT_SECONDARY
                            }))
                            .child(SharedString::from(text)),
                    );
                }
            }
            _ => {
                output = output.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "batch.run.output.empty".to_owned())
                        .child(ux.batch_run_output_empty),
                );
            }
        }
        column = column.child(output).child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(|| "batch.run.timeline".to_owned())
                .child(components::section_label(ux.batch_run_timeline, metrics)),
        );
        let Some(record) = page.run().host(host) else {
            return column;
        };
        let start = page.run().started_at_unix();
        for (index, step) in record.steps().iter().enumerate() {
            let time = page.time_of(host, step.index());
            let command = page
                .confirmed()
                .task()
                .steps()
                .get(index)
                .map(|step| step.command().to_owned())
                .unwrap_or_default();
            let offsets = match (time.started, time.finished) {
                (Some(started), Some(finished)) => format!(
                    "{} → {}",
                    offset_text(start, started),
                    offset_text(start, finished)
                ),
                (Some(started), None) => offset_text(start, started),
                _ => String::new(),
            };
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .debug_selector(move || format!("batch.run.timeline.{index}"))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(copy::fill(
                                ux.batch_run_step,
                                &[("index", &(index + 1).to_string())],
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(command)),
                    )
                    .child(components::chip(
                        step_state_text(step.state(), ux),
                        step_state_tone(step.state()),
                        metrics,
                    ))
                    .when(!offsets.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(SharedString::from(offsets)),
                        )
                    }),
            );
        }
        column
    }
}

/// The sentence under the library that counts the tasks the list did not draw.
///
/// It is the library's own sentence - `batch_library_more` - and never the one a batch
/// preview uses for its hosts: what is counted under this list are saved tasks, and a
/// sentence about "还有 N 台" would count machines. It is a function rather than an
/// expression inside the drawing code so that a test can hold the drawn row to that
/// sentence instead of only to the wording of the constant.
pub(crate) fn library_more_line(ux: &UxStrings, hidden: usize) -> SharedString {
    SharedString::from(copy::fill(
        ux.batch_library_more,
        &[("count", &hidden.to_string())],
    ))
}

/// One target of the preview.
fn batch_target_row(target: &BatchTarget, index: usize, metrics: Metrics) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("batch.targets.row.{index}")))
        .flex()
        .items_center()
        .flex_none()
        .gap(metrics.scale(spacing::TIGHT))
        .debug_selector(move || format!("batch.targets.row.{index}"))
        .child(
            div()
                .flex_none()
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(SharedString::from(target.label().to_owned())),
        )
        .child(components::endpoint(target.destination(), metrics))
}

/// A clock offset from the run's start, which is what a timeline reads as.
fn offset_text(start: u64, at: u64) -> String {
    format!("+{}s", at.saturating_sub(start))
}

/// Debug selector of one failure policy segment.
fn batch_policy_selector(policy: FailurePolicy) -> &'static str {
    match policy {
        FailurePolicy::SkipHost => "batch.failure.skip",
        FailurePolicy::StopAfterFailure => "batch.failure.stop",
    }
}

/// The sentence for a run state.
fn run_state_text(state: RunState, ux: &UxStrings) -> &'static str {
    match state {
        RunState::Running => ux.batch_run_running,
        RunState::Paused => ux.batch_run_paused,
        RunState::Cancelling => ux.batch_run_cancelling,
        RunState::Completed => ux.batch_run_completed,
        RunState::Failed => ux.batch_run_failed,
        RunState::Cancelled => ux.batch_run_cancelled,
        RunState::Interrupted => ux.batch_run_interrupted,
    }
}

/// The tone of a run state: a state is never told by colour alone, but the colour has to
/// agree with the word beside it.
fn run_state_tone(state: RunState) -> Tone {
    match state {
        RunState::Running => Tone::Accent,
        RunState::Paused | RunState::Cancelling => Tone::Warning,
        RunState::Completed => Tone::Positive,
        RunState::Failed | RunState::Interrupted => Tone::Danger,
        RunState::Cancelled => Tone::Neutral,
    }
}

/// The sentence for one host's state.
fn host_state_text(state: HostState, ux: &UxStrings) -> &'static str {
    match state {
        HostState::Queued => ux.batch_host_queued,
        HostState::Connecting => ux.batch_host_connecting,
        HostState::Running => ux.batch_host_running,
        HostState::WaitingForUser => ux.batch_host_waiting,
        HostState::Succeeded => ux.batch_host_succeeded,
        HostState::Failed => ux.batch_host_failed,
        HostState::Cancelled => ux.batch_host_cancelled,
        HostState::Skipped => ux.batch_host_skipped,
        HostState::Interrupted => ux.batch_host_interrupted,
    }
}

/// The tone of one host's state.
fn host_state_tone(state: HostState) -> Tone {
    match state {
        HostState::Queued | HostState::Connecting | HostState::Running => Tone::Accent,
        HostState::WaitingForUser => Tone::Warning,
        HostState::Succeeded => Tone::Positive,
        HostState::Failed | HostState::Interrupted => Tone::Danger,
        HostState::Cancelled | HostState::Skipped => Tone::Neutral,
    }
}

/// The sentence for one step's state.
fn step_state_text(state: StepState, ux: &UxStrings) -> &'static str {
    match state {
        StepState::Pending => ux.batch_step_pending,
        StepState::Running => ux.batch_step_running,
        StepState::Succeeded => ux.batch_step_succeeded,
        StepState::Failed => ux.batch_step_failed,
        StepState::Skipped => ux.batch_step_skipped,
        StepState::Cancelled => ux.batch_step_cancelled,
        StepState::AlreadyDone => ux.batch_step_already_done,
        StepState::Interrupted => ux.batch_step_interrupted,
    }
}

/// The tone of one step's state.
fn step_state_tone(state: StepState) -> Tone {
    match state {
        StepState::Pending | StepState::AlreadyDone => Tone::Neutral,
        StepState::Running => Tone::Accent,
        StepState::Succeeded => Tone::Positive,
        StepState::Failed | StepState::Interrupted => Tone::Danger,
        StepState::Skipped | StepState::Cancelled => Tone::Neutral,
    }
}
// ---------------------------------------------------------------------------
// A run with no network (tests and the capture)
// ---------------------------------------------------------------------------

/// A batch run that produces real runner state without a network.
///
/// The runner, its scheduler, its events, the fold and the page are the production ones;
/// only the transport is a fixture. That is what makes a shot of a running page evidence
/// about this page rather than about a mock: [BatchRunner] really ran, and the host, step
/// and output states came from it.
///
/// The one addition is a gate: a fixture command can hold until the gate is released, so
/// the state a shot shows does not move between the two frames the capture compares. A
/// cancelled run keeps saying cancelling until the command ends, which the gate decides,
/// so that state can be photographed instead of raced.
#[cfg(any(test, feature = "ui-capture"))]
pub mod fixture {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    use shellune_core::{BatchTarget, OutputStream};
    use shellune_ssh::batch::{HostControl, HostTransport, StepFailure, TransportFactory};

    /// The handle the caller of a fixture keeps: while it is held, held commands wait.
    pub struct Gate {
        inner: Arc<GateInner>,
    }

    impl Gate {
        /// A closed gate.
        #[must_use]
        pub fn new() -> Self {
            Self {
                inner: Arc::new(GateInner {
                    open: Mutex::new(false),
                    ready: Condvar::new(),
                }),
            }
        }

        /// Lets every waiting command end, and every later one too.
        pub fn release(&self) {
            let mut open = match self.inner.open.lock() {
                Ok(open) => open,
                Err(poisoned) => poisoned.into_inner(),
            };
            *open = true;
            self.inner.ready.notify_all();
        }
    }

    impl Default for Gate {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for Gate {
        fn drop(&mut self) {
            // A fixture that goes away must not leave a command blocked forever: the
            // threads that would wait belong to the runner this fixture started.
            self.release();
        }
    }

    pub(super) struct GateInner {
        open: Mutex<bool>,
        ready: Condvar,
    }

    fn wait(gate: &GateInner) {
        let mut open = match gate.open.lock() {
            Ok(open) => open,
            Err(poisoned) => poisoned.into_inner(),
        };
        while !*open {
            open = match gate.ready.wait(open) {
                Ok(open) => open,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// What one command of the fixture does.
    #[derive(Clone, Debug, Default)]
    pub struct StepScript {
        /// What it writes to the standard output.
        pub stdout: String,
        /// What it writes to the error stream.
        pub stderr: String,
        /// The exit status it reports.
        pub exit: u32,
        /// Whether it waits for the gate before it ends.
        pub hold: bool,
    }

    impl StepScript {
        /// A command that prints one line and succeeds.
        #[must_use]
        pub fn ok(stdout: impl Into<String>) -> Self {
            Self {
                stdout: stdout.into(),
                ..Self::default()
            }
        }

        /// A command that prints an error and fails with a status.
        #[must_use]
        pub fn failing(exit: u32, stderr: impl Into<String>) -> Self {
            Self {
                stderr: stderr.into(),
                exit,
                ..Self::default()
            }
        }

        /// A command that prints one line and then waits for the gate.
        #[must_use]
        pub fn holding(stdout: impl Into<String>) -> Self {
            Self {
                stdout: stdout.into(),
                hold: true,
                ..Self::default()
            }
        }
    }

    /// What one host of the fixture runs, in order.
    #[derive(Clone, Debug, Default)]
    pub struct HostScript {
        /// Its steps.
        pub steps: Vec<StepScript>,
    }

    impl HostScript {
        /// A host that runs the given steps.
        #[must_use]
        pub fn new(steps: Vec<StepScript>) -> Self {
            Self { steps }
        }
    }

    /// Which host runs what, by the label the task snapshot carries.
    #[derive(Clone, Debug, Default)]
    pub struct Plan {
        hosts: BTreeMap<String, HostScript>,
    }

    impl Plan {
        /// A plan with no host in it yet.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Gives one host its script.
        #[must_use]
        pub fn host(mut self, label: impl Into<String>, script: HostScript) -> Self {
            self.hosts.insert(label.into(), script);
            self
        }

        /// Gives every named host the same script.
        #[must_use]
        pub fn every(mut self, labels: &[&str], script: HostScript) -> Self {
            for label in labels {
                self.hosts.insert((*label).to_owned(), script.clone());
            }
            self
        }
    }

    /// The factory that hands the runner one fixture transport per target.
    pub struct Factory {
        plan: Arc<Plan>,
        gate: Arc<GateInner>,
        opens: Arc<AtomicUsize>,
    }

    impl Factory {
        /// A factory for a plan and the gate its commands wait on.
        #[must_use]
        pub fn new(plan: Arc<Plan>, gate: &Gate) -> Self {
            Self {
                plan,
                gate: Arc::clone(&gate.inner),
                opens: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// How many transports the runner asked for, which is how many hosts it tried to
        /// reach. A preview that was never confirmed leaves this at zero.
        #[cfg(test)]
        #[must_use]
        pub fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

    impl TransportFactory for Factory {
        fn open(&self, target: &BatchTarget) -> Result<Box<dyn HostTransport>, String> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let script = self
                .plan
                .hosts
                .get(target.label())
                .cloned()
                .unwrap_or_default();
            Ok(Box::new(Transport {
                script,
                at: 0,
                gate: Arc::clone(&self.gate),
            }))
        }
    }

    struct Transport {
        script: HostScript,
        at: usize,
        gate: Arc<GateInner>,
    }

    impl HostTransport for Transport {
        fn connect(&mut self, _control: &HostControl) -> Result<(), String> {
            Ok(())
        }

        fn exec(
            &mut self,
            _command: &str,
            control: &HostControl,
            output: &mut dyn FnMut(OutputStream, &[u8]),
        ) -> Result<Option<u32>, StepFailure> {
            let step = self
                .script
                .steps
                .get(self.at)
                .cloned()
                .unwrap_or_else(|| StepScript::ok(""));
            self.at += 1;
            if !step.stdout.is_empty() {
                output(OutputStream::Stdout, step.stdout.as_bytes());
            }
            if !step.stderr.is_empty() {
                output(OutputStream::Stderr, step.stderr.as_bytes());
            }
            if step.hold {
                wait(&self.gate);
            }
            if control.is_cancelled() {
                return Err(StepFailure::Cancelled);
            }
            Ok(Some(step.exit))
        }

        fn disconnect(&mut self) {}
    }
}

#[cfg(any(test, feature = "ui-capture"))]
impl ShelluneView {
    /// The batch panel, for a test or a scenario that reads the state a shot is of.
    pub fn batch_panel(&self) -> &crate::app::batch::BatchPanel {
        &self.batch
    }

    /// Empties the selection, so a test can show what a frozen preview keeps.
    #[cfg(test)]
    pub fn capture_batch_clear_selection(&mut self, cx: &mut Context<Self>) {
        self.hosts.clear_selection();
        cx.notify();
    }

    /// Narrows the list to a group, as the tree does (R18).
    #[cfg(test)]
    pub fn capture_batch_filter(&mut self, path: &str, cx: &mut Context<Self>) {
        self.hosts
            .set_group_filter(shellune_core::GroupPath::parse(path).ok());
        cx.notify();
    }

    /// Opens the preview for what the selection and the scope mean right now.
    pub fn capture_batch_preview(&mut self, cx: &mut Context<Self>) {
        self.open_batch_preview(cx);
    }

    /// Selects the first hosts the filters keep, as the list's own control does (R18).
    pub fn capture_batch_select(&mut self, count: usize, cx: &mut Context<Self>) {
        let list = self
            .hosts
            .list(&self.book, &self.groups, &|id| self.host_is_open(id));
        let picked: Vec<HostId> = list
            .ids_in_scope(self.hosts.scope())
            .iter()
            .copied()
            .take(count)
            .collect();
        self.hosts.select(picked);
        cx.notify();
    }

    /// Confirms the preview with a fixture that never dials.
    ///
    /// Returns the factory, so a caller can show that no transport was opened before the
    /// confirmation and one per host after it, and the gate that lets the fixture's
    /// commands end.
    pub fn capture_batch_start(
        &mut self,
        plan: fixture::Plan,
        cx: &mut Context<Self>,
    ) -> (std::sync::Arc<fixture::Factory>, fixture::Gate) {
        let gate = fixture::Gate::new();
        let factory = std::sync::Arc::new(fixture::Factory::new(std::sync::Arc::new(plan), &gate));
        let shared: std::sync::Arc<dyn shellune_ssh::batch::TransportFactory> = factory.clone();
        let _ = self.batch.confirm(cx, shared);
        self.batch_polling = false;
        (factory, gate)
    }

    /// Types one command per step into the preview, adding the steps a person would add.
    ///
    /// # Errors
    ///
    /// Returns the model's refusal when a step cannot be added, such as the step limit.
    #[cfg(feature = "ui-capture")]
    pub fn capture_batch_commands(
        &mut self,
        commands: &[&str],
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        while self.batch.steps().len() < commands.len() {
            self.batch.add_step(cx).map_err(|error| error.to_string())?;
        }
        for (field, command) in self.batch.steps().iter().zip(commands) {
            field.update(cx, |field, cx| field.set_text(*command, cx));
        }
        cx.notify();
        Ok(())
    }

    /// Pauses the run on screen.
    #[cfg(feature = "ui-capture")]
    pub fn capture_batch_pause(&mut self, cx: &mut Context<Self>) {
        self.pause_batch(cx);
    }

    /// Asks the run on screen to stop.
    #[cfg(feature = "ui-capture")]
    pub fn capture_batch_cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_batch(cx);
    }

    /// Opens the saved tasks, for a test or a shot.
    pub fn capture_batch_library(&mut self, cx: &mut Context<Self>) {
        self.open_batch_library(cx);
    }

    /// Opens the stored runs, for a test or a shot.
    pub fn capture_batch_history(&mut self, cx: &mut Context<Self>) {
        self.open_batch_history(cx);
    }

    /// Opens the detail of one stored run, by its place in the list the filter keeps.
    pub fn capture_batch_stored(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let id = self
            .batch
            .history_visible()
            .get(index)
            .map(|entry| entry.id());
        match id {
            Some(id) => {
                self.open_stored_batch(id, cx);
                true
            }
            None => false,
        }
    }

    /// Saves the definition in the editor without running it.
    ///
    /// # Errors
    ///
    /// Returns the model's refusal or the reason the file could not be written.
    #[cfg(test)]
    pub fn capture_batch_save(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let outcome = self.batch.save_draft(cx);
        cx.notify();
        outcome.map(|_| ())
    }

    /// Chooses one host of the run page, as its list does.
    #[cfg(test)]
    pub fn capture_batch_select_host(
        &mut self,
        host: shellune_core::HostId,
        cx: &mut Context<Self>,
    ) {
        self.batch.select_host(host);
        cx.notify();
    }

    /// Leaves the run page with the run still going.
    pub fn capture_batch_background(&mut self, cx: &mut Context<Self>) {
        self.background_batch(cx);
    }

    /// Confirms the editor with a fixture, handing the answer back.
    ///
    /// The other helpers start a run and ignore the refusal; this one returns it, so a
    /// test can drive the branch that answers a second confirmation (R13).
    #[cfg(test)]
    pub fn capture_batch_confirm(
        &mut self,
        plan: fixture::Plan,
        cx: &mut Context<Self>,
    ) -> (Result<(), crate::app::batch::ConfirmRefusal>, fixture::Gate) {
        let gate = fixture::Gate::new();
        let factory = std::sync::Arc::new(fixture::Factory::new(std::sync::Arc::new(plan), &gate));
        let shared: std::sync::Arc<dyn shellune_ssh::batch::TransportFactory> = factory.clone();
        let outcome = self.batch.confirm(cx, shared);
        self.batch_polling = false;
        cx.notify();
        (outcome, gate)
    }

    /// Reads the runner until the page is in a state the caller names, or until the
    /// attempts run out.
    ///
    /// This waits for a fixture command that has already been scripted, not for a
    /// network: what it waits for is the runner's own report.
    pub fn capture_batch_wait(
        &mut self,
        cx: &mut Context<Self>,
        mut done: impl FnMut(&ShelluneView) -> bool,
    ) -> bool {
        for _ in 0..600 {
            if done(self) {
                self.batch.normalize_times_for_capture();
                return true;
            }
            self.poll_batch(cx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let arrived = done(self);
        if arrived {
            self.batch.normalize_times_for_capture();
        }
        arrived
    }
}
