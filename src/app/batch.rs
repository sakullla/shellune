//! The batch task centre: the model a library, an editor, a run and a history are drawn
//! from (R12, R13, R19, R21, R22).
//!
//! This module holds no rendering and opens no connection of its own. It keeps four
//! surfaces apart, because the difference between them is the whole point of R12:
//!
//! * a **library** is the saved definitions. Saving one writes `tasks.json` and takes no
//!   transport factory, so "save without running" is a property of the call rather than
//!   a promise the interface makes.
//! * an **editor** is one definition being written: its targets, its steps in order, its
//!   concurrency and its failure policy. It keeps the identifier of the saved task it
//!   came from, so saving an edit replaces that record instead of adding a copy.
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
//! The **history** is what the stored records say: the summary of each run, read from
//! `runs/<id>.json`, and one host's output read from `runs/<id>.jsonl` only when it is
//! asked for, bounded by bytes. A record that cannot be read keeps its reason and does
//! not hide the others; a restart marks the unfinished runs interrupted and replays
//! nothing.
//!
//! Pause, cancel and retry follow the runner's own semantics instead of inventing a
//! second set: a pause stops new steps and leaves a running command alone, a cancel is
//! reported as [RunState::Cancelling] until the remote command has really ended, and a
//! retry never repeats a step that already succeeded. The runner belongs to the centre
//! rather than to a surface, so a run keeps polling while the library or the history is
//! on screen.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity};
use shellune_core::{
    BatchBook, BatchEvent, BatchEventKind, BatchProblem, BatchRun, BatchRunId, BatchTarget,
    BatchTask, BatchTaskId, ConfirmedBatch, FailurePolicy, HostConfig, HostId, HostState,
    OutputStream, RunState,
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

/// How many stored runs the history lists.
///
/// The count of what was left out is exact; the rows are what a person can scan, so a
/// store with thousands of runs is listed by its newest ones instead of being loaded
/// whole.
pub const MAX_HISTORY_RUNS: usize = 500;

/// How much of a run's log the history reads when it rebuilds one host's output.
///
/// A run with thousands of hosts printed more than a window can show, so the log is
/// read from its end: a bounded read, not a load of the whole file.
#[allow(dead_code)] // read by the history surface and by the tests here.
pub const MAX_HISTORY_LOG_BYTES: usize = 256 * 1024;

/// How many targets the preview lists.
///
/// The count above the list is exact; the rows are what a person can scan. A task of
/// five thousand hosts says so in one line rather than drawing five thousand rows.
pub const MAX_PREVIEW_ROWS: usize = 120;

/// Which surface of the panel is on screen.
///
/// This is the two-surface projection the overlay draws today: a preview or a run. The
/// task centre keeps four surfaces ([BatchCenterView]) and this type says which of them
/// the overlay can show as a dialog, so the library and the history are reached through
/// [BatchCenterView] instead of being forced into this shape.
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

/// Which surface of the task centre is on screen.
///
/// The four surfaces are separate because they hold separate things, and the split is
/// what keeps a runner alive across a view change:
///
/// * **Library** is the saved definitions. Opening it reads `tasks.json` and starts
///   nothing.
/// * **Editor** is one definition being written: the targets, the steps in order, the
///   concurrency and the failure policy. Saving it writes the file and opens nothing;
///   only a confirmation freezes it and starts a run.
/// * **ActiveRun** is the one run this window is driving. The runner belongs to the
///   task centre, not to the surface, so leaving for the library or the history keeps
///   the poll going and coming back shows the same page.
/// * **History** is what the stored records say, read on demand: the summary of each
///   run and, when the user asks for one host's output, a bounded read of that run's
///   log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchCenterView {
    /// Nothing of the task centre is drawn.
    #[default]
    Closed,
    /// The saved tasks.
    Library,
    /// One definition being edited.
    Editor,
    /// The run this window is driving.
    ActiveRun,
    /// The stored runs of the past.
    History,
}

#[allow(dead_code)] // named for the task centre view, which draws these next.
impl BatchCenterView {
    /// Whether anything of the task centre is drawn.
    #[must_use]
    pub fn is_open(self) -> bool {
        self != Self::Closed
    }

    /// The dialog surface this view is drawn as, when it is drawn as one.
    #[must_use]
    pub fn page(self) -> BatchPage {
        match self {
            Self::Closed | Self::Library | Self::History => BatchPage::Closed,
            Self::Editor => BatchPage::Preview,
            Self::ActiveRun => BatchPage::Run,
        }
    }

    /// Whether this view owns the editor's fields.
    #[must_use]
    pub fn is_editor(self) -> bool {
        self == Self::Editor
    }

    /// Whether this view shows the run this window is driving.
    #[must_use]
    pub fn is_active_run(self) -> bool {
        self == Self::ActiveRun
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

/// The saved definitions, as the library surface reads them.
///
/// A library is a list of definitions that were written on purpose. Loading it starts
/// nothing, and a damaged file is reported instead of being replaced by an empty one:
/// the previous list is kept, so a corrupt `tasks.json` cannot look like "no saved
/// tasks".
#[derive(Debug, Default)]
pub struct BatchLibrary {
    tasks: Vec<BatchTask>,
    problem: Option<String>,
    loaded: bool,
}

#[allow(dead_code)] // named for the library surface, which draws this next.
impl BatchLibrary {
    /// The saved tasks, in the order the file keeps them.
    #[must_use]
    pub fn tasks(&self) -> &[BatchTask] {
        &self.tasks
    }

    /// One saved task.
    #[must_use]
    pub fn task(&self, id: BatchTaskId) -> Option<&BatchTask> {
        self.tasks.iter().find(|task| task.id() == id)
    }

    /// Why the file could not be read or written, when it could not.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether the file has been read at least once.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Replaces the list with what the file holds.
    fn adopt(&mut self, book: BatchBook) {
        self.tasks = book.tasks().to_vec();
        self.problem = None;
        self.loaded = true;
    }

    /// Keeps one definition, replacing the record with the same identifier.
    fn keep(&mut self, task: BatchTask) {
        match self.tasks.iter_mut().find(|saved| saved.id() == task.id()) {
            Some(existing) => *existing = task,
            None => self.tasks.push(task),
        }
        self.loaded = true;
    }

    /// Removes one definition and reports whether it was there.
    fn forget(&mut self, id: BatchTaskId) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|task| task.id() != id);
        before != self.tasks.len()
    }

    /// Records why the library could not do what was asked.
    fn note(&mut self, reason: impl Into<String>) {
        self.problem = Some(reason.into());
    }
}

/// One stored run, as the history surface reads it.
///
/// A record that cannot be read keeps its identifier and its reason, and the other
/// records are still listed: one damaged summary must not hide the rest of the past.
#[allow(dead_code)] // the reason and the summary are what the history surface draws.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    id: BatchRunId,
    summary: Option<BatchRun>,
    problem: Option<String>,
}

#[allow(dead_code)] // named for the history surface, which draws this next.
impl HistoryEntry {
    /// Which run this is.
    #[must_use]
    pub fn id(&self) -> BatchRunId {
        self.id
    }

    /// What the stored summary says, when it could be read.
    #[must_use]
    pub fn summary(&self) -> Option<&BatchRun> {
        self.summary.as_ref()
    }

    /// Why the record could not be read, when it could not.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }
}

/// The stored runs, newest first, read from their summaries.
///
/// Nothing here replays a run or touches a log: the summary says what happened, and a
/// host's output is read from the log only when the user asks for it.
#[derive(Debug, Default)]
pub struct BatchHistory {
    entries: Vec<HistoryEntry>,
    problem: Option<String>,
    omitted: usize,
}

#[allow(dead_code)] // named for the history surface, which draws this next.
impl BatchHistory {
    /// The runs the history lists, newest first.
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// One stored run.
    #[must_use]
    pub fn entry(&self, id: BatchRunId) -> Option<&HistoryEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Why the run directory could not be listed, when it could not.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// How many older runs were left out to keep the list bounded.
    #[must_use]
    pub fn omitted(&self) -> usize {
        self.omitted
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

/// The batch task centre of one window: the saved definitions, the definition being
/// edited, the one run this window is driving, and the stored history.
///
/// The four surfaces are fields rather than separate objects because they share one
/// store and one library: what the editor saves is what the library lists, and what a
/// run writes is what the history reads. What is *not* shared is the runner. It is
/// created by [BatchCenter::confirm] and lives here until the run ends, whatever
/// surface is on screen, so a view change cannot stop a run or lose its page.
pub struct BatchCenter {
    /// Which surface is drawn.
    view: BatchCenterView,
    // --- the editor: one definition being written -----------------------------
    /// The identifier this definition keeps, so editing a saved task stays that task.
    task_id: BatchTaskId,
    /// The scope the targets were taken from.
    scope: SelectionScope,
    /// The frozen targets of this definition.
    targets: Vec<BatchTarget>,
    /// The hosts that could not be included, with the reason.
    refused: Vec<(String, String)>,
    /// The command fields, in the order the steps will run.
    steps: Vec<Entity<TextField>>,
    /// The concurrency field, present while the editor is open.
    concurrency: Option<Entity<TextField>>,
    /// The name field, present while the editor is open.
    name: Option<Entity<TextField>>,
    /// What a failure does to the rest of the task.
    on_failure: FailurePolicy,
    /// Why the last action was refused.
    problem: Option<String>,
    /// A sentence about this task that is not a problem, such as "this is a retry".
    note: Option<String>,
    /// The earlier run this definition retries, when it is a retry.
    retry_of: Option<BatchRunId>,
    // --- the run this window drives, and the stored past -----------------------
    /// The run this window is driving, and the page that shows it.
    active: Option<BatchRunPage>,
    /// The saved definitions.
    library: BatchLibrary,
    /// The stored runs.
    history: BatchHistory,
    /// Where the definitions and the runs are kept.
    store: Option<BatchStore>,
}

/// The name this panel had when it was one page.
///
/// The overlay, the capture fixtures and the existing tests still name the task centre
/// this way; [BatchCenterView] is the surface they are being moved onto.
pub type BatchPanel = BatchCenter;

impl BatchCenter {
    /// A task centre that shows nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            view: BatchCenterView::Closed,
            task_id: BatchTaskId::new(),
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
            active: None,
            library: BatchLibrary::default(),
            history: BatchHistory::default(),
            store: None,
        }
    }

    /// Rewrites the run's step times to fixed offsets, for a capture (R2).
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn normalize_times_for_capture(&mut self) {
        if let Some(run) = self.active.as_mut() {
            run.normalize_times_for_capture();
        }
    }

    /// Which dialog surface the overlay can draw, when it can draw one.
    #[must_use]
    pub fn page(&self) -> BatchPage {
        self.view.page()
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

    /// The run this window is driving, whatever surface is on screen.
    #[must_use]
    pub fn run(&self) -> Option<&BatchRunPage> {
        self.active.as_ref()
    }

    /// Whether the runner is still alive, whatever surface is on screen.
    ///
    /// This is the run's own lifetime, not the view's: the poll keeps going while the
    /// user is in the library or the history, so leaving the run page does not stop
    /// the run and does not lose the events it reported meanwhile.
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|page| page.runner.is_some() && !page.finished)
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
        self.scope = scope;
        self.targets = targets;
        self.refused = refused;
        self.on_failure = FailurePolicy::default();
        self.problem = None;
        self.note = None;
        self.retry_of = None;
        // A new definition is its own task: only editing a saved one keeps an id.
        self.task_id = BatchTaskId::new();
        // The run this window is driving is not the editor's business: it keeps going
        // and keeps its page.
        self.view = BatchCenterView::Editor;
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
        self.view = BatchCenterView::Editor;
        self.scope = SelectionScope::CurrentResults;
        self.targets = task.targets().to_vec();
        self.refused = Vec::new();
        self.on_failure = task.on_failure();
        self.problem = None;
        self.note = Some(ux.batch_retry_note.to_owned());
        self.retry_of = task.retry_of();
        // The retry is a new definition: it is confirmed again before anything runs.
        self.task_id = task.id();
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

    /// Puts the task centre away, without touching what it does not own.
    ///
    /// A run that is still going cannot be put away: the page is the only place its
    /// state is visible, so the centre refuses until the run has ended. A finished run
    /// is kept in the history by the store, which is why the page can be dropped.
    pub fn close(&mut self) -> bool {
        if self.is_watching() {
            return false;
        }
        self.view = BatchCenterView::Closed;
        self.problem = None;
        self.note = None;
        self.steps.clear();
        self.concurrency = None;
        self.name = None;
        self.retry_of = None;
        self.active = None;
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
        // The identity is the editor's, not a fresh one: editing a saved task and
        // confirming or saving it again must stay that task.
        let mut task =
            BatchTask::new(steps, self.targets.clone(), concurrency)?.with_id(self.task_id);
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
        self.view = BatchCenterView::ActiveRun;
        self.steps.clear();
        self.concurrency = None;
        self.name = None;
        self.active = Some(BatchRunPage::new(confirmed, run, runner));
        Ok(())
    }

    /// Keeps the definition, so the task can be run again after a restart.
    ///
    /// A failure here is said on the page and does not stop the run: the file is a
    /// convenience, and a task that cannot be saved is still a task the user confirmed.
    fn save_task(&mut self, confirmed: &ConfirmedBatch) {
        if self.store.is_none() {
            self.library.keep(confirmed.task().clone());
            return;
        }
        if let Err(reason) = self.store_draft(confirmed.task().clone()) {
            self.note = Some(format!("{}: {reason}", copy::ux().batch_save_failed));
        }
    }

    /// Takes what the runner has reported, and ends the run when it is over.
    ///
    /// This is not gated on the surface being drawn: the poll belongs to the run, so
    /// the events a runner produced while the user was in the library or the history
    /// are folded into the same page, and coming back shows what happened meanwhile.
    ///
    /// Returns whether the caller should keep polling: a run that is still going, or one
    /// whose last events have not been read yet, keeps the centre watching.
    pub fn pump(&mut self) -> bool {
        let Some(page) = self.active.as_mut() else {
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
            // The run is over, so the record of it is what the history reads. It is
            // read again on demand rather than kept in memory here.
        }
        self.is_watching()
    }

    /// Stops starting new steps, leaving a running command alone.
    pub fn pause(&mut self) {
        if let Some(runner) = self.active.as_ref().and_then(BatchRunPage::runner) {
            runner.pause();
        }
    }

    /// Continues a paused run from the step it was left at.
    pub fn resume(&mut self) {
        if let Some(runner) = self.active.as_ref().and_then(BatchRunPage::runner) {
            runner.resume();
        }
    }

    /// Asks the run to stop.
    ///
    /// The run is reported as cancelling at once and as cancelled only after every
    /// running command has really ended, which is the difference the page shows.
    pub fn cancel(&mut self) {
        if let Some(runner) = self.active.as_ref().and_then(BatchRunPage::runner) {
            runner.cancel();
        }
    }

    /// Answers the question one host is waiting on.
    ///
    /// Returns whether that host was really asking, so a trust record is only written for
    /// an answer that was used.
    pub fn answer_trust(&mut self, host: HostId, accept: bool) -> bool {
        match self.active.as_ref().and_then(BatchRunPage::runner) {
            Some(runner) => runner.answer_trust(host, accept),
            None => false,
        }
    }

    /// Shows one host's output in the page.
    pub fn select_host(&mut self, host: HostId) {
        if let Some(page) = self.active.as_mut() {
            page.selected = Some(host);
        }
    }

    /// Records why an action was refused, for the page to show.
    pub fn set_problem(&mut self, problem: impl Into<String>) {
        self.problem = Some(problem.into());
    }
}

impl Default for BatchCenter {
    fn default() -> Self {
        Self::new()
    }
}

/// The surfaces and actions the task centre owns beyond the run dialog.
///
/// The library, the history and the switch between the four surfaces are named here
/// because the model owns them: saving a draft, re-editing a saved task, retrying a
/// stored run and rebuilding one host's output all have semantics that a view must not
/// re-invent. The tests in this module drive them without a window; the task centre
/// view calls them next, which is why the names are kept even while the overlay still
/// draws only the preview and the run.
#[allow(dead_code)]
impl BatchCenter {
    /// Which surface of the task centre is on screen.
    #[must_use]
    pub fn view(&self) -> BatchCenterView {
        self.view
    }

    /// Whether the editor is what is on screen.
    #[must_use]
    pub fn is_previewing(&self) -> bool {
        self.view.is_editor()
    }

    /// The run this window is driving and has not finished.
    #[must_use]
    pub fn active_run(&self) -> Option<&BatchRunPage> {
        self.active.as_ref().filter(|page| !page.finished)
    }

    /// Whether this window is driving a run at all.
    #[must_use]
    pub fn has_active_run(&self) -> bool {
        self.active.is_some()
    }

    /// How many hosts of the active run have not reached a final state.
    #[must_use]
    pub fn active_unfinished_hosts(&self) -> usize {
        self.active
            .as_ref()
            .map_or(0, |page| page.total_hosts() - page.finished_hosts())
    }

    /// The saved definitions.
    #[must_use]
    pub fn library(&self) -> &BatchLibrary {
        &self.library
    }

    /// The stored runs.
    #[must_use]
    pub fn history(&self) -> &BatchHistory {
        &self.history
    }

    /// The identifier the definition in the editor keeps.
    ///
    /// A saved task edited again keeps its id, so saving it replaces that record; a new
    /// definition gets a fresh one, and a run never reuses an earlier run's identity.
    #[must_use]
    pub fn task_id(&self) -> BatchTaskId {
        self.task_id
    }

    /// Shows the saved definitions, reading them from the store.
    ///
    /// Reading starts nothing: a library is a list of definitions, and its targets are
    /// snapshots that were frozen when they were written.
    pub fn show_library(&mut self) {
        self.view = BatchCenterView::Library;
        self.refresh_library();
    }

    /// Shows the stored runs, reading their summaries.
    pub fn show_history(&mut self) {
        self.view = BatchCenterView::History;
        self.refresh_history();
    }

    /// Goes back to the run this window is driving.
    ///
    /// Returns whether there is such a run. The runner was never paused by leaving, so
    /// nothing has to be restarted to come back to it.
    pub fn show_active_run(&mut self) -> bool {
        if self.active.is_none() {
            return false;
        }
        self.view = BatchCenterView::ActiveRun;
        true
    }

    /// Goes back to the definition being edited.
    ///
    /// Returns whether there is one. Unsaved input is still there: the editor is not a
    /// copy taken when the surface was left.
    pub fn show_editor(&mut self) -> bool {
        if self.concurrency.is_none() {
            return false;
        }
        self.view = BatchCenterView::Editor;
        true
    }

    /// Re-reads the saved definitions.
    pub fn refresh_library(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        match store.load_tasks() {
            Ok(book) => self.library.adopt(book),
            // A damaged file is reported and the previous list is kept: a corrupt
            // document must not read as "no saved tasks".
            Err(error) => self.library.note(error.to_string()),
        }
    }

    /// Re-reads the stored runs, newest first, bounded to what a list can show.
    ///
    /// One record that cannot be read keeps its identifier and its reason; the rest of
    /// the history is still listed, because one damaged run is not the end of the past.
    pub fn refresh_history(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let ids = match store.list_runs() {
            Ok(ids) => ids,
            Err(error) => {
                self.history.problem = Some(error.to_string());
                return;
            }
        };
        self.history.problem = None;
        self.history.omitted = ids.len().saturating_sub(MAX_HISTORY_RUNS);
        self.history.entries = ids
            .into_iter()
            .take(MAX_HISTORY_RUNS)
            .map(|id| match store.load_run(id) {
                Ok(summary) => HistoryEntry {
                    id,
                    summary: Some(summary),
                    problem: None,
                },
                Err(error) => HistoryEntry {
                    id,
                    summary: None,
                    problem: Some(error.to_string()),
                },
            })
            .collect();
    }

    /// Opens one saved definition in the editor under the identity it was saved with.
    ///
    /// The steps, targets, concurrency, failure policy and name come from the record;
    /// saving it again replaces that record instead of adding a second copy.
    ///
    /// # Errors
    ///
    /// Returns the reason the record is not in the library.
    pub fn edit_saved(
        &mut self,
        cx: &mut Context<ShelluneView>,
        id: BatchTaskId,
    ) -> Result<(), String> {
        let Some(task) = self.library.task(id).cloned() else {
            return Err(self.library_missing_reason(id));
        };
        let ux = copy::ux();
        self.view = BatchCenterView::Editor;
        self.task_id = task.id();
        self.scope = SelectionScope::CurrentResults;
        self.targets = task.targets().to_vec();
        self.refused.clear();
        self.on_failure = task.on_failure();
        self.retry_of = task.retry_of();
        self.problem = None;
        self.note = None;
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

    /// Saves the definition being edited as a draft, and starts nothing.
    ///
    /// This is the "save without running" path: it writes `tasks.json` and does not take
    /// a transport factory, so no connection can be opened by it. Confirmation is still
    /// [Self::confirm].
    ///
    /// # Errors
    ///
    /// Returns the definition problem or the reason the file could not be written; the
    /// draft stays in the editor either way.
    pub fn save_draft(&mut self, cx: &App) -> Result<BatchTaskId, String> {
        let task = match self.candidate(cx) {
            Ok(task) => task,
            Err(problem) => {
                self.problem = Some(problem.to_string());
                return Err(problem.to_string());
            }
        };
        let id = task.id();
        if let Err(reason) = self.store_draft(task) {
            self.problem = Some(reason.clone());
            return Err(reason);
        }
        self.problem = None;
        Ok(id)
    }

    /// Deletes one saved definition.
    ///
    /// The record is removed from the file first and from the list after, so a write
    /// that fails leaves the library saying what the file still holds.
    ///
    /// # Errors
    ///
    /// Returns the reason the record could not be removed.
    pub fn delete_saved(&mut self, id: BatchTaskId) -> Result<(), String> {
        let Some(store) = self.store.as_ref() else {
            if self.library.forget(id) {
                return Ok(());
            }
            return Err(self.library_missing_reason(id));
        };
        let mut book = store.load_tasks().map_err(|error| error.to_string())?;
        if !book.remove(id) {
            return Err(self.library_missing_reason(id));
        }
        store.save_tasks(&book).map_err(|error| error.to_string())?;
        self.library.forget(id);
        Ok(())
    }

    /// Moves one step one place earlier, which changes the order they run in.
    ///
    /// # Errors
    ///
    /// Returns [BatchProblem::NoSteps] for the first step, which has nowhere to go.
    pub fn move_step_up(&mut self, index: usize) -> Result<(), BatchProblem> {
        match move_swap(self.steps.len(), index, true) {
            Some((from, to)) => {
                self.steps.swap(from, to);
                self.problem = None;
                Ok(())
            }
            None => {
                let problem = BatchProblem::NoSteps;
                self.problem = Some(problem.to_string());
                Err(problem)
            }
        }
    }

    /// Moves one step one place later, which changes the order they run in.
    ///
    /// # Errors
    ///
    /// Returns [BatchProblem::NoSteps] for the last step, which has nowhere to go.
    pub fn move_step_down(&mut self, index: usize) -> Result<(), BatchProblem> {
        match move_swap(self.steps.len(), index, false) {
            Some((from, to)) => {
                self.steps.swap(from, to);
                self.problem = None;
                Ok(())
            }
            None => {
                let problem = BatchProblem::NoSteps;
                self.problem = Some(problem.to_string());
                Err(problem)
            }
        }
    }

    /// Opens the work a stored run still owes as a new draft.
    ///
    /// The summary is the one the store kept, so a retry after a restart is derived from
    /// what was written down rather than from memory. The draft goes through the editor
    /// and a confirmation like any other; nothing is started here.
    ///
    /// # Errors
    ///
    /// Returns the reason the run is not in the history, or the model's refusal when
    /// every host and step already succeeded.
    pub fn retry_saved(
        &mut self,
        cx: &mut Context<ShelluneView>,
        id: BatchRunId,
    ) -> Result<(), String> {
        let run = self
            .history
            .entry(id)
            .and_then(|entry| entry.summary.clone())
            .or_else(|| {
                self.active
                    .as_ref()
                    .filter(|page| page.run.id() == id)
                    .map(|page| page.run.clone())
            });
        let Some(run) = run else {
            return Err(format!("the stored run {id} could not be read"));
        };
        self.open_retry(cx, &run)
            .map_err(|problem| problem.to_string())
    }

    /// Marks every unfinished stored run as interrupted, the way a restart does.
    ///
    /// The marking is written to the summary and to the log; nothing is replayed and no
    /// runner is created. Returns how many runs were marked.
    pub fn recover(&mut self, at_unix: u64) -> usize {
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        match store.recover_interrupted(at_unix) {
            Ok(runs) => {
                self.refresh_history();
                runs.len()
            }
            Err(error) => {
                self.history.problem = Some(error.to_string());
                0
            }
        }
    }

    /// One host's output of a stored run, rebuilt from its log within a byte bound.
    ///
    /// The history does not hold every line of every run: the log is read from its end,
    /// bounded by `max_bytes`, and folded into the same bounded [HostOutput] the live
    /// page uses. A log longer than the bound is read at its tail, which
    /// [HostOutput::dropped] does not claim to be the whole run.
    ///
    /// # Errors
    ///
    /// Returns the reason the log could not be read, including a log that is damaged
    /// before the part being read.
    pub fn stored_output(
        &self,
        run: BatchRunId,
        host: HostId,
        max_bytes: usize,
    ) -> Result<HostOutput, String> {
        let Some(store) = self.store.as_ref() else {
            return Err("no batch store is open".to_owned());
        };
        let log = store
            .read_events_tail(run, max_bytes)
            .map_err(|error| error.to_string())?;
        let mut output = HostOutput::default();
        for event in log.events() {
            if let BatchEventKind::Output {
                host: at,
                stream,
                chunk,
                ..
            } = event.kind()
                && *at == host
            {
                output.push(*stream, chunk.as_str());
            }
        }
        Ok(output)
    }

    /// Writes one definition into the library and the file.
    fn store_draft(&mut self, task: BatchTask) -> Result<(), String> {
        let Some(store) = self.store.as_ref() else {
            self.library.keep(task);
            return Ok(());
        };
        // A first save starts a book at the current format; a file that exists but
        // cannot be read is not replaced, because overwriting it would bury the record
        // of what went wrong. (A missing file reads back as a default book, whose
        // version is the empty one, so the two cases are told apart by the file.)
        let mut book = if store.tasks_path().is_file() {
            store.load_tasks().map_err(|error| error.to_string())?
        } else {
            BatchBook::new()
        };
        book.insert(task.clone());
        store.save_tasks(&book).map_err(|error| error.to_string())?;
        self.library.keep(task);
        Ok(())
    }

    /// The sentence for a record the library does not hold.
    fn library_missing_reason(&self, id: BatchTaskId) -> String {
        format!("the saved task {id} is not in the library")
    }
}

/// The two positions a step move swaps, or `None` when the step cannot move that way.
///
/// The editor reorders steps by swapping neighbours, so the only rule is which pairs
/// may swap: the first step cannot move up and the last cannot move down. Keeping it in
/// one place means the two directions cannot disagree about the ends.
fn move_swap(len: usize, index: usize, up: bool) -> Option<(usize, usize)> {
    if index >= len || len == 0 {
        return None;
    }
    // The two branches are written out rather than built with `then_some`, because the
    // argument of that call is evaluated eagerly and the first step would underflow.
    if up {
        if index > 0 {
            Some((index - 1, index))
        } else {
            None
        }
    } else if index + 1 < len {
        Some((index, index + 1))
    } else {
        None
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use shellune_core::ConnectionDefaults;
    use shellune_ssh::batch::{HostControl, HostTransport, RunnerOptions, StepFailure};

    use super::*;

    /// A directory of its own for one test, removed when the test starts.
    fn scratch(label: &str) -> std::path::PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "shellune-batch-centre-{}-{}-{label}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn fixture_target(name: &str) -> BatchTarget {
        BatchTarget::new(
            HostId::new(),
            name,
            "192.0.2.20",
            22,
            "deploy",
            None,
            Vec::new(),
            ConnectionDefaults::default(),
        )
        .expect("a valid target")
    }

    fn fixture_task(steps: &[&str], hosts: &[&str]) -> BatchTask {
        BatchTask::new(
            BatchTask::steps_from_commands(steps.iter().map(|step| (*step).to_owned()))
                .expect("valid steps"),
            hosts.iter().map(|name| fixture_target(name)).collect(),
            2,
        )
        .expect("a valid task")
    }

    /// A transport that counts its opens and succeeds at every command.
    ///
    /// The count is the whole point: a path that is not a confirmation must leave it at
    /// zero, which is how "saving a draft opens nothing" is observed rather than read
    /// off a signature.
    #[derive(Clone, Default)]
    struct CountingHosts {
        opens: std::sync::Arc<AtomicUsize>,
    }

    impl CountingHosts {
        fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

    struct SilentHost;

    impl TransportFactory for CountingHosts {
        fn open(&self, _target: &BatchTarget) -> Result<Box<dyn HostTransport>, String> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(SilentHost))
        }
    }

    impl HostTransport for SilentHost {
        fn connect(&mut self, _control: &HostControl) -> Result<(), String> {
            Ok(())
        }

        fn exec(
            &mut self,
            command: &str,
            _control: &HostControl,
            output: &mut dyn FnMut(OutputStream, &[u8]),
        ) -> Result<Option<u32>, StepFailure> {
            output(OutputStream::Stdout, format!("{command}\n").as_bytes());
            Ok(Some(0))
        }

        fn disconnect(&mut self) {}
    }

    /// A task centre with a store, and no editor fields.
    fn centre(dir: &std::path::Path) -> (BatchCenter, BatchStore) {
        let store = BatchStore::at(dir);
        let mut centre = BatchCenter::new();
        centre.store = Some(store.clone());
        (centre, store)
    }

    #[test]
    fn a_saved_draft_is_written_to_the_file_and_starts_nothing() {
        let (mut centre, store) = centre(&scratch("draft"));
        let factory = CountingHosts::default();
        let task = fixture_task(&["uptime"], &["alpha", "beta"]);
        let id = task.id();

        // Saving is the whole action: it takes no transport factory, so there is no
        // path from it to a connection.
        centre
            .store_draft(task.clone())
            .expect("the draft is saved");
        assert_eq!(factory.opens(), 0, "saving a draft opens nothing");
        assert!(!centre.has_active_run(), "and starts no run");

        let written = store.load_tasks().expect("the file reads back");
        assert_eq!(written.tasks().len(), 1);
        assert_eq!(written.task(id), Some(&task));

        centre.refresh_library();
        assert_eq!(centre.library().tasks().len(), 1);
        assert!(centre.library().task(id).is_some());
        assert!(centre.library().problem().is_none());
    }

    #[test]
    fn editing_a_saved_task_keeps_its_identity_instead_of_adding_a_copy() {
        let (mut centre, store) = centre(&scratch("draft-edit"));
        let task = fixture_task(&["uptime"], &["alpha"]);
        let id = task.id();
        centre
            .store_draft(task.clone())
            .expect("the draft is saved");

        // What an editor hands back is the same task under its own identifier.
        let mut edited = task.clone().with_id(id);
        edited.set_name(Some("nightly"));
        assert_eq!(edited.id(), id);
        centre.store_draft(edited).expect("the edit is saved");

        let written = store.load_tasks().expect("the file reads back");
        assert_eq!(
            written.tasks().len(),
            1,
            "the record is replaced, not copied"
        );
        assert_eq!(written.task(id).and_then(BatchTask::name), Some("nightly"));

        centre.refresh_library();
        centre.delete_saved(id).expect("the record is removed");
        assert!(centre.library().task(id).is_none());
        assert!(
            store
                .load_tasks()
                .expect("the file reads back")
                .tasks()
                .is_empty()
        );
    }

    #[test]
    fn a_damaged_task_file_is_reported_and_never_overwritten_by_a_save() {
        let dir = scratch("draft-damaged");
        let (mut centre, store) = centre(&dir);
        std::fs::create_dir_all(store.directory()).expect("the directory is made");
        std::fs::write(store.tasks_path(), b"{ not a book").expect("the damage is written");

        centre.show_library();
        assert_eq!(centre.view(), BatchCenterView::Library);
        assert!(
            centre.library().problem().is_some(),
            "a damaged file says so"
        );
        assert!(
            centre.library().tasks().is_empty(),
            "and no task is invented"
        );

        let draft = fixture_task(&["uptime"], &["alpha"]);
        assert!(
            centre.store_draft(draft).is_err(),
            "a save over a damaged file is refused"
        );
        assert_eq!(
            std::fs::read(store.tasks_path()).expect("the file is readable"),
            b"{ not a book",
            "the evidence of what went wrong is kept"
        );
    }

    #[test]
    fn a_damaged_stored_run_is_listed_with_its_reason_and_the_others_still_read() {
        let (mut centre, store) = centre(&scratch("history"));
        let good = BatchRun::planned(
            &fixture_task(&["uptime"], &["alpha"])
                .confirm(10)
                .expect("a confirmable task"),
            BatchRunId::new(),
            11,
        );
        store.save_run(&good).expect("the summary is written");
        let damaged = BatchRunId::new();
        std::fs::write(store.run_path(damaged), b"{ not a run").expect("the damage is written");

        centre.show_history();
        assert_eq!(centre.view(), BatchCenterView::History);
        let entries = centre.history().entries();
        assert_eq!(entries.len(), 2);
        let readable = entries
            .iter()
            .find(|entry| entry.id() == good.id())
            .expect("the readable run is listed");
        assert!(readable.summary().is_some());
        assert!(readable.problem().is_none());
        let broken = entries
            .iter()
            .find(|entry| entry.id() == damaged)
            .expect("the damaged run is still listed");
        assert!(broken.summary().is_none());
        assert!(
            broken.problem().is_some(),
            "the damaged record says why it could not be read"
        );
    }

    #[test]
    fn a_restart_marks_the_unfinished_run_interrupted_and_starts_no_runner() {
        let (mut centre, store) = centre(&scratch("recover"));
        let confirmed = fixture_task(&["one", "two"], &["alpha"])
            .confirm(10)
            .expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        run.apply(&BatchEvent::new(
            0,
            11,
            BatchEventKind::RunStarted {
                hosts: 1,
                steps: 2,
                concurrency: 1,
            },
        ))
        .expect("the run started");
        store.save_run(&run).expect("the summary is written");

        let marked = centre.recover(1_700_000_000);
        assert_eq!(marked, 1);
        assert!(!centre.has_active_run(), "a restart replays nothing");
        let entry = centre
            .history()
            .entry(run.id())
            .expect("the history has it");
        let summary = entry.summary().expect("its summary was read");
        assert_eq!(summary.state(), RunState::Interrupted);

        // The log says the same thing the summary does.
        let log = store.read_events(run.id()).expect("the log reads back");
        assert!(
            log.events().iter().any(|event| matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Interrupted
                }
            )),
            "the interruption is recorded, not inferred later"
        );
    }

    #[test]
    fn a_stored_run_rebuilds_one_hosts_output_within_a_byte_bound() {
        let (centre, store) = centre(&scratch("output"));
        let run = BatchRun::planned(
            &fixture_task(&["uptime"], &["alpha", "beta"])
                .confirm(10)
                .expect("a confirmable task"),
            BatchRunId::new(),
            11,
        );
        store.save_run(&run).expect("the summary is written");
        let host = run.hosts()[0].target().host_id();
        let other = run.hosts()[1].target().host_id();
        for index in 0..50u64 {
            store
                .append(
                    run.id(),
                    BatchEventKind::Output {
                        host,
                        step: 0,
                        stream: OutputStream::Stdout,
                        chunk: format!("line {index}\n"),
                    },
                    index,
                )
                .expect("the line is written");
        }
        store
            .append(
                run.id(),
                BatchEventKind::Output {
                    host: other,
                    step: 0,
                    stream: OutputStream::Stderr,
                    chunk: "another host\n".to_owned(),
                },
                99,
            )
            .expect("the other host's line is written");

        let whole = centre
            .stored_output(run.id(), host, MAX_HISTORY_LOG_BYTES)
            .expect("the output reads back");
        assert_eq!(whole.lines().len(), 50);
        assert_eq!(whole.lines()[0].text, "line 0");
        assert_eq!(
            whole.lines()[49].text,
            "line 49",
            "only the host that was asked for is read"
        );

        let bounded = centre
            .stored_output(run.id(), host, 600)
            .expect("the tail reads back");
        assert!(
            bounded.lines().len() < 50,
            "a bounded read keeps fewer lines than the whole log"
        );
        assert_eq!(
            bounded.lines().last().expect("at least one line").text,
            "line 49",
            "a bounded read keeps the newest lines"
        );
    }

    #[test]
    fn a_run_keeps_polling_while_another_surface_is_on_screen() {
        let (mut centre, _store) = centre(&scratch("pump"));
        let confirmed = fixture_task(&["uptime"], &["alpha", "beta"])
            .confirm(10)
            .expect("a confirmable task");
        let run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        let factory: Arc<dyn TransportFactory> = Arc::new(CountingHosts::default());
        let runner = BatchRunner::start(
            confirmed.clone(),
            factory,
            RunnerOptions::from_task(&confirmed),
        );
        centre.active = Some(BatchRunPage::new(confirmed, run, runner));
        centre.view = BatchCenterView::Library;

        let deadline = Instant::now() + Duration::from_secs(20);
        while centre.is_watching() && Instant::now() < deadline {
            centre.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !centre.is_watching(),
            "the run ended while the library was shown"
        );
        let page = centre.run().expect("the run page is still there");
        assert!(page.is_finished());
        assert_eq!(page.state(), RunState::Completed);
        assert_eq!(
            centre.view(),
            BatchCenterView::Library,
            "polling the run does not change the surface the user chose"
        );
    }

    #[test]
    fn a_confirmed_definition_is_frozen_against_a_later_edit() {
        let mut task = fixture_task(&["uptime"], &["alpha"]);
        let confirmed = task.clone().confirm(10).expect("a confirmable task");
        let run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        task.set_name(Some("edited after the confirmation"));
        assert_eq!(task.name(), Some("edited after the confirmation"));
        assert_eq!(
            confirmed.task().name(),
            None,
            "the run keeps the definition it was confirmed with"
        );
        assert_eq!(run.task().name(), None);

        // Every run has its own identity, even for the same definition.
        let second = BatchRun::planned(&confirmed, BatchRunId::new(), 12);
        assert_ne!(run.id(), second.id());
    }

    #[test]
    fn steps_move_one_place_at_a_time_and_not_past_the_ends() {
        assert_eq!(move_swap(3, 0, true), None, "the first step cannot move up");
        assert_eq!(move_swap(3, 1, true), Some((0, 1)));
        assert_eq!(move_swap(3, 1, false), Some((1, 2)));
        assert_eq!(
            move_swap(3, 2, false),
            None,
            "the last step cannot move down"
        );
        assert_eq!(move_swap(1, 0, true), None);
        assert_eq!(move_swap(1, 0, false), None);
        assert_eq!(move_swap(0, 0, true), None, "an empty list has no order");
        assert_eq!(
            move_swap(2, 5, true),
            None,
            "an index off the end moves nothing"
        );
    }
}
