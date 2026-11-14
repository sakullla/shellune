//! The batch task panel: the model a preview, a run and a retry are drawn from (R19, R21).
//!
//! This module holds no rendering and opens no connection of its own. It keeps three
//! things apart, because the difference between them is the whole point of R19:
//!
//! * a **preview** is a snapshot of hosts plus the commands that would run on them. It
//!   is built from the saved book, the selection and the same scope semantics the host
//!   list uses (R18), and it can be shown, edited and refused without any side effect.
//!   Nothing here can dial: a preview holds [BatchTarget] values, not sessions.
//! * a **run** starts only from a [ConfirmedBatch], which only [BatchTask::confirm]
//!   produces. So "no confirmation, no session" is a property of the types rather than
//!   a rule the interface has to remember, and a test can show a preview with a factory
//!   that counts opens and observe none.
//! * a **retry** is the [BatchTask] that [BatchTask::retry_after] derives from a
//!   finished run: the hosts that did not succeed and, per host, only the steps still
//!   owed. It goes back through the preview so the user confirms what will run again.
//!   Nothing claims a rollback: whatever ran on the remote machine stays there.
//!
//! The run's state is the fold of the events the runner produced ([BatchRun::apply]),
//! which is the same fold the stored summary is rebuilt from. Two things are added on
//! top for the page: a bounded per-host output log (a command may print more than a
//! window can show) and per-step times for the timeline.
//!
//! Pause, cancel and retry follow the runner's own semantics instead of inventing a
//! second set: a pause stops new steps and leaves a running command alone, a cancel is
//! reported as [RunState::Cancelling] until the remote command has really ended, and a
//! retry never repeats a step that already succeeded.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity};
use shellune_core::{
    BatchEvent, BatchEventKind, BatchProblem, BatchRun, BatchRunId, BatchTarget, BatchTask,
    ConfirmedBatch, FailurePolicy, HostConfig, HostId, HostState, OutputStream, RunState,
};
use shellune_ssh::batch::{BatchRunner, HostRoute, RouteSource, RunnerOptions, TransportFactory};
use shellune_storage::{BatchStore, HostBook, HostTrust, SecretStore};

use crate::app::copy;
use crate::app::hosts::SelectionScope;
use crate::app::remote;
use crate::app::text_field::TextField;
use crate::app::view::ShelluneView;

/// How often the page asks the runner what happened.
///
/// Short enough that a step's output appears while it is still running, long enough
/// that an idle run costs almost nothing.
pub const BATCH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// How many output lines one host keeps on screen.
///
/// A command can print for an hour; the page keeps what a person can read and counts
/// the rest instead of holding a log in the window. The run's file keeps every line.
pub const MAX_OUTPUT_LINES: usize = 400;

/// How many targets the preview lists.
///
/// The count above the list is exact; the rows are what a person can scan. A task of
/// five thousand hosts says so in one line rather than drawing five thousand rows.
pub const MAX_PREVIEW_ROWS: usize = 120;

/// Which surface of the panel is on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchPage {
    /// No batch surface: the page underneath is unobstructed.
    #[default]
    Closed,
    /// The preview: targets, commands, concurrency and failure policy, before anything
    /// is confirmed.
    Preview,
    /// The run: per-host state, output and the timeline.
    Run,
}

impl BatchPage {
    /// Whether anything of the panel is drawn.
    #[must_use]
    pub fn is_open(self) -> bool {
        self != Self::Closed
    }
}

/// One line of a host's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLine {
    /// Which stream it came from.
    pub stream: OutputStream,
    /// The text, without its line break.
    pub text: String,
}

impl OutputLine {
    /// Whether this line came from the error stream, which the page tints apart.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.stream == OutputStream::Stderr
    }
}

/// What one host printed, bounded.
#[derive(Clone, Debug, Default)]
pub struct HostOutput {
    lines: Vec<OutputLine>,
    dropped: usize,
    /// The last line was written without a line break, so the next chunk continues it.
    open: bool,
}

impl HostOutput {
    /// The lines kept, oldest first.
    #[must_use]
    pub fn lines(&self) -> &[OutputLine] {
        &self.lines
    }

    /// How many earlier lines were dropped to stay inside [MAX_OUTPUT_LINES].
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Appends one chunk, continuing the last line when it was not ended.
    fn push(&mut self, stream: OutputStream, chunk: &str) {
        const NEWLINE: char = '\n';
        for part in chunk.split_inclusive(NEWLINE) {
            let ends = part.ends_with(NEWLINE);
            let text = part.strip_suffix(NEWLINE).unwrap_or(part);
            match self.lines.last_mut() {
                Some(last) if self.open && last.stream == stream => {
                    last.text.push_str(text);
                }
                _ => self.lines.push(OutputLine {
                    stream,
                    text: text.to_owned(),
                }),
            }
            self.open = !ends;
        }
        while self.lines.len() > MAX_OUTPUT_LINES {
            self.lines.remove(0);
            self.dropped += 1;
        }
    }
}

/// When one step of one host started and ended.
///
/// The page shows them as offsets from the run's start, which is what makes "which host
/// was slow" readable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepTime {
    /// When the step started, in Unix seconds.
    pub started: Option<u64>,
    /// When it ended, in Unix seconds.
    pub finished: Option<u64>,
}

/// A question for the user about one host.
///
/// The runner stops that host and keeps the others moving; the page turns the question
/// into two buttons whose answer goes straight back to the runner. Nothing is accepted
/// on the user's behalf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchQuestion {
    /// The host presented an identity that is not the trusted one.
    HostKey {
        /// The key algorithm.
        algorithm: String,
        /// The SHA-256 fingerprint to compare.
        fingerprint: String,
    },
    /// A ProxyCommand would run on this machine.
    ProxyCommand {
        /// The command text.
        command: String,
    },
}

impl BatchQuestion {
    /// The sentence the row shows, which is the question the user is answering.
    #[must_use]
    pub fn text(&self, ux: &copy::UxStrings) -> String {
        match self {
            Self::HostKey {
                algorithm,
                fingerprint,
            } => copy::fill(
                ux.batch_trust_question,
                &[("algorithm", algorithm), ("fingerprint", fingerprint)],
            ),
            Self::ProxyCommand { command } => {
                copy::fill(ux.batch_proxy_question, &[("command", command)])
            }
        }
    }
}

/// Everything the run page shows: the fold of the events, the output, the times.
pub struct BatchRunPage {
    run: BatchRun,
    confirmed: ConfirmedBatch,
    runner: Option<Box<BatchRunner>>,
    outputs: BTreeMap<HostId, HostOutput>,
    times: BTreeMap<(HostId, u32), StepTime>,
    questions: BTreeMap<HostId, BatchQuestion>,
    selected: Option<HostId>,
    problem: Option<String>,
    finished: bool,
}

impl BatchRunPage {
    /// A page for a run that has just been started.
    pub fn new(confirmed: ConfirmedBatch, run: BatchRun, runner: BatchRunner) -> Self {
        let selected = confirmed.task().targets().first().map(BatchTarget::host_id);
        Self {
            run,
            confirmed,
            runner: Some(Box::new(runner)),
            outputs: BTreeMap::new(),
            times: BTreeMap::new(),
            questions: BTreeMap::new(),
            selected,
            problem: None,
            finished: false,
        }
    }

    /// The model the page reads: the state of every host and every step.
    #[must_use]
    pub fn run(&self) -> &BatchRun {
        &self.run
    }

    /// The frozen definition this run executes.
    #[must_use]
    pub fn confirmed(&self) -> &ConfirmedBatch {
        &self.confirmed
    }

    /// What the run is doing, or what it ended as.
    #[must_use]
    pub fn state(&self) -> RunState {
        self.run.state()
    }

    /// Whether the run reached its end.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// How many hosts reached a final state.
    #[must_use]
    pub fn finished_hosts(&self) -> usize {
        self.run.finished_hosts()
    }

    /// How many hosts the run has.
    #[must_use]
    pub fn total_hosts(&self) -> usize {
        self.run.hosts().len()
    }

    /// How many hosts are working right now.
    #[must_use]
    pub fn running_hosts(&self) -> usize {
        self.run
            .hosts()
            .iter()
            .filter(|host| {
                matches!(
                    host.state(),
                    HostState::Connecting | HostState::Running | HostState::WaitingForUser
                )
            })
            .count()
    }

    /// How many hosts failed.
    #[must_use]
    pub fn failed_hosts(&self) -> usize {
        self.run
            .hosts()
            .iter()
            .filter(|host| host.state() == HostState::Failed)
            .count()
    }

    /// The host whose output the page shows.
    #[must_use]
    pub fn selected(&self) -> Option<HostId> {
        self.selected
    }

    /// What one host printed, when it printed anything.
    #[must_use]
    pub fn output(&self, host: HostId) -> Option<&HostOutput> {
        self.outputs.get(&host)
    }

    /// When one step started and ended.
    #[must_use]
    pub fn time_of(&self, host: HostId, step: u32) -> StepTime {
        self.times.get(&(host, step)).copied().unwrap_or_default()
    }

    /// Rewrites the step times to fixed offsets from the run's start, for a capture.
    ///
    /// The timeline is drawn from the runner's real clock, which makes the last digit of
    /// an offset differ between two runs of the same revision. A shot has to be
    /// comparable with the next one, so the fixture states the offsets instead (R2).
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn normalize_times_for_capture(&mut self) {
        let start = self.run.started_at_unix();
        for ((_, step), time) in self.times.iter_mut() {
            let offset = u64::from(*step);
            time.started = time.started.map(|_| start + offset);
            time.finished = time.finished.map(|_| start + offset + 1);
        }
    }

    /// The question one host is waiting on, when it is waiting.
    #[must_use]
    pub fn question(&self, host: HostId) -> Option<&BatchQuestion> {
        self.questions.get(&host)
    }

    /// Why the page could not do what was asked, when it could not.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether a retry would have anything to run.
    #[must_use]
    pub fn can_retry(&self) -> bool {
        self.finished
            && self
                .run
                .hosts()
                .iter()
                .any(|host| host.state() != HostState::Succeeded)
    }

    /// Folds one batch of events into the page, writing each one to the run's log.
    ///
    /// The summary, the log, the output and the times move together, so they cannot
    /// show different stories about the same run.
    pub fn apply(&mut self, events: &[BatchEvent], store: Option<&BatchStore>) {
        for event in events {
            if let Some(store) = store {
                // The log is the record that survives a crash. A write that fails is
                // reported on the page; it is not a reason to stop the run.
                if let Err(error) =
                    store.append(self.run.id(), event.kind().clone(), event.at_unix())
                {
                    self.problem = Some(format!("{}: {error}", copy::ux().batch_log_failed));
                }
            }
            self.apply_one(event.kind());
        }
    }

    /// Folds one event kind in.
    ///
    /// It is public so a capture or a test can build the page from a recorded sequence
    /// of events without a network, through exactly the fold a live run uses.
    pub fn apply_one(&mut self, kind: &BatchEventKind) {
        match kind {
            BatchEventKind::StepStarted { host, step } => {
                let time = self.times.entry((*host, *step)).or_default();
                time.started = Some(remote::now_unix());
            }
            BatchEventKind::StepFinished { host, step, .. } => {
                let time = self.times.entry((*host, *step)).or_default();
                time.finished = Some(remote::now_unix());
                self.questions.remove(host);
            }
            BatchEventKind::Output {
                host,
                stream,
                chunk,
                ..
            } => {
                self.outputs
                    .entry(*host)
                    .or_default()
                    .push(*stream, chunk.as_str());
            }
            BatchEventKind::TrustPending {
                host,
                algorithm,
                fingerprint,
                ..
            } => {
                self.questions.insert(
                    *host,
                    BatchQuestion::HostKey {
                        algorithm: algorithm.clone(),
                        fingerprint: fingerprint.clone(),
                    },
                );
            }
            BatchEventKind::ProxyPending { host, command } => {
                self.questions.insert(
                    *host,
                    BatchQuestion::ProxyCommand {
                        command: command.clone(),
                    },
                );
            }
            BatchEventKind::TrustDecided { host, .. }
            | BatchEventKind::HostFinished { host, .. } => {
                self.questions.remove(host);
            }
            BatchEventKind::RunStarted { .. }
            | BatchEventKind::RunState { .. }
            | BatchEventKind::HostState { .. } => {}
        }
        // A refused transition is the model saying no. The page keeps what it has and
        // says so instead of showing a state the log does not contain.
        if let Err(error) = self.run.apply_kind(kind) {
            self.problem = Some(error.to_string());
        }
    }

    fn take_events(&mut self) -> Vec<BatchEvent> {
        match self.runner.as_mut() {
            Some(runner) => runner.drain(),
            None => Vec::new(),
        }
    }

    fn runner_finished(&self) -> bool {
        self.runner
            .as_ref()
            .is_some_and(|runner| runner.is_finished())
    }

    /// Gives the runner back, so a caller can ask it what happened one last time.
    fn take_runner(&mut self) -> Option<Box<BatchRunner>> {
        self.runner.take()
    }

    fn runner(&self) -> Option<&BatchRunner> {
        self.runner.as_deref()
    }
}

/// The batch panel of one window: the preview, its fields, and the run on screen.
pub struct BatchPanel {
    page: BatchPage,
    scope: SelectionScope,
    targets: Vec<BatchTarget>,
    refused: Vec<(String, String)>,
    steps: Vec<Entity<TextField>>,
    concurrency: Option<Entity<TextField>>,
    name: Option<Entity<TextField>>,
    on_failure: FailurePolicy,
    problem: Option<String>,
    note: Option<String>,
    retry_of: Option<BatchRunId>,
    run: Option<BatchRunPage>,
    watching: bool,
    store: Option<BatchStore>,
}

impl BatchPanel {
    /// A panel that shows nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            page: BatchPage::Closed,
            scope: SelectionScope::CurrentResults,
            targets: Vec::new(),
            refused: Vec::new(),
            steps: Vec::new(),
            concurrency: None,
            name: None,
            on_failure: FailurePolicy::default(),
            problem: None,
            note: None,
            retry_of: None,
            run: None,
            watching: false,
            store: None,
        }
    }

    /// Rewrites the run's step times to fixed offsets, for a capture (R2).
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn normalize_times_for_capture(&mut self) {
        if let Some(run) = self.run.as_mut() {
            run.normalize_times_for_capture();
        }
    }

    /// Which surface is on screen.
    #[must_use]
    pub fn page(&self) -> BatchPage {
        self.page
    }

    /// Whether the preview is what is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn is_previewing(&self) -> bool {
        self.page == BatchPage::Preview
    }

    /// The scope the targets were taken from.
    #[must_use]
    pub fn scope(&self) -> SelectionScope {
        self.scope
    }

    /// The frozen targets of this preview.
    #[must_use]
    pub fn targets(&self) -> &[BatchTarget] {
        &self.targets
    }

    /// How many hosts the task would run on: the number the preview promises.
    #[must_use]
    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    /// The hosts that could not be included, with the reason.
    #[must_use]
    pub fn refused(&self) -> &[(String, String)] {
        &self.refused
    }

    /// The command fields, in the order the steps will run.
    #[must_use]
    pub fn steps(&self) -> &[Entity<TextField>] {
        &self.steps
    }

    /// The concurrency field, present while the preview is open.
    #[must_use]
    pub fn concurrency(&self) -> Option<&Entity<TextField>> {
        self.concurrency.as_ref()
    }

    /// The name field, present while the preview is open.
    #[must_use]
    pub fn name(&self) -> Option<&Entity<TextField>> {
        self.name.as_ref()
    }

    /// What a failure does to the rest of the task.
    #[must_use]
    pub fn on_failure(&self) -> FailurePolicy {
        self.on_failure
    }

    /// Why the preview or the run could not do what was asked.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// A sentence about this task that is not a problem, such as "this is a retry".
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// The run on screen, when a run is what is on screen.
    #[must_use]
    pub fn run(&self) -> Option<&BatchRunPage> {
        self.run.as_ref()
    }

    /// Whether the page is still waiting for the runner to finish.
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.watching
    }

    /// Opens the preview for a snapshot of targets.
    ///
    /// The targets are the ones the caller already resolved from the book and the
    /// selection, so the snapshot is taken once, when the preview opens, and an edit to
    /// a host or a group afterwards cannot change what this preview shows. The commands
    /// start with one empty step: a task with no step is refused by the model, and the
    /// field is where the user has to answer it.
    pub fn open_preview(
        &mut self,
        cx: &mut Context<ShelluneView>,
        scope: SelectionScope,
        targets: Vec<BatchTarget>,
        refused: Vec<(String, String)>,
        store: Option<BatchStore>,
    ) {
        let ux = copy::ux();
        self.page = BatchPage::Preview;
        self.scope = scope;
        self.targets = targets;
        self.refused = refused;
        self.on_failure = FailurePolicy::default();
        self.problem = None;
        self.note = None;
        self.retry_of = None;
        self.run = None;
        self.watching = false;
        self.store = store;
        self.steps = Vec::new();
        self.add_step_quietly(cx, ux.batch_step_placeholder);
        self.concurrency = Some(cx.new(|cx| {
            TextField::with_text("", shellune_core::DEFAULT_BATCH_CONCURRENCY.to_string(), cx)
        }));
        self.name = Some(cx.new(|cx| TextField::new(ux.batch_name_placeholder, cx)));
    }

    /// Opens the preview for the work a finished run still owes.
    ///
    /// The task comes from [BatchTask::retry_after], so it holds exactly the hosts that
    /// did not succeed and, per host, only the steps that did not already succeed. It
    /// is a new task, not a rollback: it goes through the preview and a confirmation
    /// like any other.
    ///
    /// # Errors
    ///
    /// Returns the model's refusal, which is [BatchProblem::NothingToRetry] when every
    /// host and every step already succeeded.
    pub fn open_retry(
        &mut self,
        cx: &mut Context<ShelluneView>,
        run: &BatchRun,
    ) -> Result<(), BatchProblem> {
        let task = BatchTask::retry_after(run)?;
        let ux = copy::ux();
        self.page = BatchPage::Preview;
        self.scope = SelectionScope::CurrentResults;
        self.targets = task.targets().to_vec();
        self.refused = Vec::new();
        self.on_failure = task.on_failure();
        self.problem = None;
        self.note = Some(ux.batch_retry_note.to_owned());
        self.retry_of = task.retry_of();
        self.run = None;
        self.watching = false;
        self.steps = Vec::new();
        for step in task.steps() {
            let command = step.command().to_owned();
            self.steps.push(cx.new(move |cx| {
                TextField::with_text(ux.batch_step_placeholder, command.clone(), cx)
            }));
        }
        if self.steps.is_empty() {
            self.add_step_quietly(cx, ux.batch_step_placeholder);
        }
        let concurrency = task.concurrency();
        self.concurrency =
            Some(cx.new(move |cx| TextField::with_text("", concurrency.to_string(), cx)));
        let name = task.name().unwrap_or_default().to_owned();
        self.name = Some(
            cx.new(move |cx| TextField::with_text(ux.batch_name_placeholder, name.clone(), cx)),
        );
        Ok(())
    }

    /// Puts the panel away, without touching what it does not own.
    ///
    /// A run that is still going cannot be put away: the page is the only place its
    /// state is visible, so the panel refuses until the run has ended.
    pub fn close(&mut self) -> bool {
        if self.page == BatchPage::Run && self.run.as_ref().is_some_and(|run| !run.finished) {
            return false;
        }
        self.page = BatchPage::Closed;
        self.problem = None;
        self.note = None;
        self.watching = false;
        self.steps.clear();
        self.concurrency = None;
        self.name = None;
        self.retry_of = None;
        self.run = None;
        self.store = None;
        self.refused.clear();
        true
    }

    fn add_step_quietly(&mut self, cx: &mut Context<ShelluneView>, placeholder: &'static str) {
        self.steps
            .push(cx.new(move |cx| TextField::new(placeholder, cx)));
    }

    /// Adds one more command step.
    ///
    /// # Errors
    ///
    /// Nothing is added past [shellune_core::MAX_BATCH_STEPS]; the reason is recorded so
    /// the page can say why the button stopped working.
    pub fn add_step(&mut self, cx: &mut Context<ShelluneView>) -> Result<(), BatchProblem> {
        let ux = copy::ux();
        if self.steps.len() >= shellune_core::MAX_BATCH_STEPS {
            let problem = BatchProblem::TooManySteps {
                max: shellune_core::MAX_BATCH_STEPS,
                found: self.steps.len() + 1,
            };
            self.problem = Some(problem.to_string());
            return Err(problem);
        }
        self.add_step_quietly(cx, ux.batch_step_placeholder);
        self.problem = None;
        Ok(())
    }

    /// Removes one command step.
    ///
    /// # Errors
    ///
    /// The last step cannot be removed: a task with no step has nothing to run, and the
    /// preview would only have to refuse it.
    pub fn remove_step(&mut self, index: usize) -> Result<(), BatchProblem> {
        if self.steps.len() <= 1 || index >= self.steps.len() {
            self.problem = Some(BatchProblem::NoSteps.to_string());
            return Err(BatchProblem::NoSteps);
        }
        self.steps.remove(index);
        self.problem = None;
        Ok(())
    }

    /// Chooses the scope the targets were taken from, before the task is confirmed.
    ///
    /// The targets themselves are not recomputed here: the caller re-snapshots them, so
    /// one place decides which hosts a scope means.
    pub fn set_scope(&mut self, scope: SelectionScope) {
        self.scope = scope;
    }

    /// Replaces the frozen targets of a preview that has not been confirmed.
    pub fn retarget(&mut self, targets: Vec<BatchTarget>, refused: Vec<(String, String)>) {
        self.targets = targets;
        self.refused = refused;
    }

    /// Sets what a failure does to the rest of the task.
    pub fn set_on_failure(&mut self, policy: FailurePolicy) {
        self.on_failure = policy;
    }

    /// The commands as typed, in order.
    #[must_use]
    pub fn commands(&self, cx: &App) -> Vec<String> {
        self.steps
            .iter()
            .map(|field| field.read(cx).text().to_owned())
            .collect()
    }

    /// The task this preview describes, built and validated by the domain.
    ///
    /// Nothing is confirmed here and nothing is started: the model is asked whether this
    /// task is runnable, and its sentence is the one the page shows.
    ///
    /// # Errors
    ///
    /// Returns the first [BatchProblem] the task has, such as an empty command with its
    /// one-based step number.
    pub fn candidate(&self, cx: &App) -> Result<BatchTask, BatchProblem> {
        let steps = BatchTask::steps_from_commands(self.commands(cx))?;
        let concurrency = self.concurrency_value(cx)?;
        let mut task = BatchTask::new(steps, self.targets.clone(), concurrency)?;
        task.set_failure_policy(self.on_failure);
        let name = self
            .name
            .as_ref()
            .map(|field| field.read(cx).text().trim().to_owned())
            .filter(|text| !text.is_empty());
        if let Some(name) = name {
            task.set_name(Some(name));
        }
        Ok(task)
    }

    /// The concurrency as typed, refused rather than guessed when it is not a number.
    ///
    /// # Errors
    ///
    /// Returns [BatchProblem::ConcurrencyOutOfRange] for text that is not a number in
    /// 1 to MAX_BATCH_CONCURRENCY; the field keeps what was typed.
    pub fn concurrency_value(&self, cx: &App) -> Result<u32, BatchProblem> {
        let text = self
            .concurrency
            .as_ref()
            .map(|field| field.read(cx).text().trim().to_owned())
            .unwrap_or_default();
        let found = text.parse::<u32>().unwrap_or(0);
        match found {
            1..=shellune_core::MAX_BATCH_CONCURRENCY => Ok(found),
            _ => Err(BatchProblem::ConcurrencyOutOfRange {
                max: shellune_core::MAX_BATCH_CONCURRENCY,
                found,
            }),
        }
    }

    /// Confirms the task and starts the run.
    ///
    /// This is the only place in the panel that can create a session, and it can only do
    /// so through a [ConfirmedBatch]: the definition is validated, frozen and then handed
    /// to a real [BatchRunner] with the caller's factory.
    ///
    /// # Errors
    ///
    /// Returns the problem the definition has; the preview stays open and nothing is
    /// started.
    pub fn confirm(
        &mut self,
        cx: &mut Context<ShelluneView>,
        factory: Arc<dyn TransportFactory>,
    ) -> Result<(), BatchProblem> {
        let task = match self.candidate(cx) {
            Ok(task) => task,
            Err(problem) => {
                self.problem = Some(problem.to_string());
                return Err(problem);
            }
        };
        let confirmed = match task.confirm(remote::now_unix()) {
            Ok(confirmed) => confirmed,
            Err(problem) => {
                self.problem = Some(problem.to_string());
                return Err(problem);
            }
        };
        self.save_task(&confirmed);
        let run = BatchRun::planned(&confirmed, BatchRunId::new(), remote::now_unix());
        let runner = BatchRunner::start(
            confirmed.clone(),
            factory,
            RunnerOptions::from_task(&confirmed),
        );
        self.problem = None;
        self.page = BatchPage::Run;
        self.watching = true;
        self.steps.clear();
        self.concurrency = None;
        self.name = None;
        self.run = Some(BatchRunPage::new(confirmed, run, runner));
        Ok(())
    }

    /// Keeps the definition, so the task can be run again after a restart.
    ///
    /// A failure here is said on the page and does not stop the run: the file is a
    /// convenience, and a task that cannot be saved is still a task the user confirmed.
    fn save_task(&mut self, confirmed: &ConfirmedBatch) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let mut book = store.load_tasks().unwrap_or_default();
        book.insert(confirmed.task().clone());
        if let Err(error) = store.save_tasks(&book) {
            self.note = Some(format!("{}: {error}", copy::ux().batch_save_failed));
        }
    }

    /// Takes what the runner has reported, and ends the run when it is over.
    ///
    /// Returns whether the caller should keep polling: a run that is still going, or one
    /// whose last events have not been read yet, keeps the page watching.
    pub fn pump(&mut self) -> bool {
        if self.page != BatchPage::Run {
            return false;
        }
        let Some(page) = self.run.as_mut() else {
            return false;
        };
        let mut events = page.take_events();
        let mut finished = false;
        if page.runner_finished() {
            // The coordinator thread has ended, so its queue holds the last events and
            // nothing else will arrive: joining is what ends the thread.
            if let Some(runner) = page.take_runner() {
                events.extend(runner.join());
                finished = true;
            }
        }
        page.apply(&events, self.store.as_ref());
        if finished && !page.finished {
            page.finished = true;
            if let Some(store) = self.store.as_ref() {
                let _ = store.save_run(&page.run);
            }
        }
        self.watching = page.runner.is_some() && !page.finished;
        self.watching
    }

    /// Stops starting new steps, leaving a running command alone.
    pub fn pause(&mut self) {
        if let Some(runner) = self.run.as_ref().and_then(BatchRunPage::runner) {
            runner.pause();
        }
    }

    /// Continues a paused run from the step it was left at.
    pub fn resume(&mut self) {
        if let Some(runner) = self.run.as_ref().and_then(BatchRunPage::runner) {
            runner.resume();
        }
    }

    /// Asks the run to stop.
    ///
    /// The run is reported as cancelling at once and as cancelled only after every
    /// running command has really ended, which is the difference the page shows.
    pub fn cancel(&mut self) {
        if let Some(runner) = self.run.as_ref().and_then(BatchRunPage::runner) {
            runner.cancel();
        }
    }

    /// Answers the question one host is waiting on.
    ///
    /// Returns whether that host was really asking, so a trust record is only written for
    /// an answer that was used.
    pub fn answer_trust(&mut self, host: HostId, accept: bool) -> bool {
        match self.run.as_ref().and_then(BatchRunPage::runner) {
            Some(runner) => runner.answer_trust(host, accept),
            None => false,
        }
    }

    /// Shows one host's output in the page.
    pub fn select_host(&mut self, host: HostId) {
        if let Some(page) = self.run.as_mut() {
            page.selected = Some(host);
        }
    }

    /// Records why an action was refused, for the page to show.
    pub fn set_problem(&mut self, problem: impl Into<String>) {
        self.problem = Some(problem.into());
    }
}

impl Default for BatchPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a run's credentials and saved hosts come from.
///
/// It resolves one target at a time, from the book and the credential store this window
/// already uses, and it asks for nothing over the network: a missing sign-in is reported
/// for that host, and the other hosts keep going.
pub struct StoredRoutes {
    hosts: Vec<HostConfig>,
    trust: HostTrust,
    secrets: Arc<dyn SecretStore>,
}

impl StoredRoutes {
    /// The routes a book, a trust file and a credential store describe.
    #[must_use]
    pub fn new(book: &HostBook, trust: &HostTrust, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            hosts: book.hosts().to_vec(),
            trust: trust.clone(),
            secrets,
        }
    }

    fn host(&self, id: HostId) -> Result<HostConfig, String> {
        self.hosts
            .iter()
            .find(|host| host.id() == id)
            .cloned()
            .ok_or_else(|| copy::ux().batch_reason_missing_host.to_owned())
    }

    fn secret(&self, host: &HostConfig) -> Option<shellune_core::SecretString> {
        if !host.remembers_secret() {
            return None;
        }
        self.secrets.load(host.id()).ok().flatten()
    }

    fn auth(&self, host: &HostConfig) -> Result<shellune_core::AuthMethod, String> {
        let secret = self.secret(host);
        remote::stored_auth(host, secret.as_ref()).ok_or_else(|| {
            copy::fill(
                copy::ux().batch_reason_no_credential,
                &[("host", host.name())],
            )
        })
    }
}

impl RouteSource for StoredRoutes {
    fn take_route(&self, target: &BatchTarget) -> Result<HostRoute, String> {
        let host = self.host(target.host_id())?;
        let jumps = target.jumps();
        if jumps.is_empty() {
            let auth = self.auth(&host)?;
            let known = remote::known_host_key(&self.trust, &host);
            return Ok(HostRoute::Direct {
                host: Box::new(host),
                auth,
                known,
            });
        }
        // A chain is dialled hop by hop, with one credential and one trust record per
        // hop: the same path an interactive connection takes.
        let mut hosts = Vec::new();
        let mut auth = Vec::new();
        for id in jumps {
            let hop = self.host(*id)?;
            auth.push(shellune_ssh::HopAuth::new(
                hop.id(),
                self.auth(&hop)?,
                remote::known_host_key(&self.trust, &hop),
            ));
            hosts.push(hop);
        }
        auth.push(shellune_ssh::HopAuth::new(
            host.id(),
            self.auth(&host)?,
            remote::known_host_key(&self.trust, &host),
        ));
        hosts.push(host);
        Ok(HostRoute::Chain {
            hosts,
            target: target.host_id(),
            auth,
        })
    }
}
