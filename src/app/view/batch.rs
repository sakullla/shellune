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
use super::shell::ModalContent;
use crate::app::batch::{BatchPage, BatchRunPage, MAX_PREVIEW_ROWS};
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

/// How many host rows a run page lists before it counts the rest.
///
/// A task can hold thousands of hosts. The count above the list is exact, so a row that
/// is not drawn is still counted; drawing thousands of rows inside a dialog would make
/// the page unusable instead.
const MAX_RUN_ROWS: usize = 200;

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
        let routes = crate::app::batch::StoredRoutes::new(
            &self.book,
            &self.trust_book,
            self.secrets.clone(),
        );
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
        let watching = self.batch.pump();
        if !watching {
            self.batch_polling = false;
        }
        cx.notify();
        watching
    }

    // -----------------------------------------------------------------------
    // The dialog
    // -----------------------------------------------------------------------

    /// The batch dialog, when one is open.
    pub(super) fn batch_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        match self.batch.page() {
            BatchPage::Closed => None,
            BatchPage::Preview => Some(self.batch_preview_dialog(window, cx)),
            BatchPage::Run => Some(self.batch_run_dialog(window, cx)),
        }
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
            .child(self.batch_targets_section(cx));
        if let Some(note) = self.batch.note() {
            body = body.child(
                components::banner(Tone::Accent, metrics)
                    .debug_selector(|| "batch.note".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(note.to_owned())),
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
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::SNUG))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.footer.note".to_owned())
                    .child(ux.batch_no_rollback),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::compact_button(ux.batch_cancel, Variant::Ghost, metrics)
                            .id("batch.preview.cancel")
                            .debug_selector(|| "batch.preview.cancel".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.close_batch(cx);
                            })),
                    )
                    .child(
                        components::button(ux.batch_confirm, Variant::Primary, metrics)
                            .id("batch.preview.confirm")
                            .debug_selector(|| "batch.preview.confirm".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.confirm_batch(cx);
                            })),
                    ),
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
            .max_h(metrics.scale(140.0))
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
                            .w(metrics.scale(20.0))
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
                            .min_w(px(0.0))
                            .debug_selector(move || format!("batch.step.field.{index}"))
                            .child(field.clone()),
                    )
                    .child(
                        components::compact_button(ux.batch_step_remove, Variant::Ghost, metrics)
                            .id(SharedString::from(format!("batch.step.remove.{index}")))
                            .debug_selector(move || format!("batch.step.remove.{index}"))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                let _ = this.batch.remove_step(index);
                                cx.notify();
                            })),
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(ux.batch_concurrency),
                    )
                    .children(concurrency.map(|field| {
                        div()
                            .flex()
                            .w(metrics.scale(72.0))
                            .debug_selector(|| "batch.concurrency".to_owned())
                            .child(field)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(copy::fill(
                                ux.batch_concurrency_hint,
                                &[("max", &shellune_core::MAX_BATCH_CONCURRENCY.to_string())],
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(ux.batch_name),
                    )
                    .children(name.map(|field| {
                        div()
                            .flex()
                            .flex_1()
                            .min_w(metrics.scale(160.0))
                            .debug_selector(|| "batch.name".to_owned())
                            .child(field)
                    })),
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
            |this: &mut Self, cx: &mut Context<Self>| this.close_batch(cx),
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
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
                .child(self.batch_run_side(page)),
        );
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::SNUG))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "batch.run.note".to_owned())
                    .child(ux.batch_no_rollback),
            )
            .child(self.batch_run_controls(page, cx));
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
        if finished {
            controls = controls.child(
                components::compact_button(ux.batch_close, Variant::Primary, metrics)
                    .id("batch.run.close.action")
                    .debug_selector(|| "batch.run.close.action".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.close_batch(cx);
                    })),
            );
        }
        controls
    }

    /// One row per host, with its steps, its exit results and its reason.
    fn batch_run_hosts(&self, page: &BatchRunPage, cx: &mut Context<Self>) -> Stateful<Div> {
        let ux = copy::ux();
        let metrics = self.metrics();
        let selected = page.selected();
        let mut list = div()
            .id("batch.run.hosts")
            .flex()
            .flex_col()
            .flex_1()
            .min_w(metrics.scale(320.0))
            .max_h(metrics.scale(300.0))
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "batch.run.hosts".to_owned());
        for (index, host) in page.run().hosts().iter().take(MAX_RUN_ROWS).enumerate() {
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
            let mut row = div()
                .id(SharedString::from(format!("batch.run.host.{index}")))
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .p(metrics.scale(spacing::TIGHT))
                .rounded(px(radius::MEDIUM))
                .bg(if is_selected {
                    theme::swatch(palette::SURFACE_SELECTED)
                } else {
                    theme::swatch(palette::SURFACE_RAISED)
                })
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
        let hidden = page.run().hosts().len().saturating_sub(MAX_RUN_ROWS);
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
        list
    }

    /// The output of one host and the times of its steps.
    fn batch_run_side(&self, page: &BatchRunPage) -> Div {
        let ux = copy::ux();
        let metrics = self.metrics();
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(metrics.scale(300.0))
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "batch.run.side".to_owned())
            .child(components::section_label(ux.batch_run_output, metrics));
        let Some(host) = page.selected() else {
            return column;
        };
        let mut output = div()
            .id("batch.run.output")
            .flex()
            .flex_col()
            .h(metrics.scale(170.0))
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HAIRLINE))
            .p(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SEGMENT_TRACK))
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
                            .min_w(px(0.0))
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
    pub fn capture_batch_pause(&mut self, cx: &mut Context<Self>) {
        self.pause_batch(cx);
    }

    /// Asks the run on screen to stop.
    pub fn capture_batch_cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_batch(cx);
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
