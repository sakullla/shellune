//! Running a confirmed batch: one host at a time per host, a bounded number of hosts
//! at once, and a log of everything that happened (R19, R21).
//!
//! The runner is a scheduler, not a script engine. It walks the frozen target list of
//! a [`ConfirmedBatch`], starts at most `concurrency` of them at once, runs each
//! host's steps one after another on one connection, and reports every step's
//! stdout, stderr, exit status and final state as events. There is no branch, no
//! variable, no step that depends on another host, and no rollback: a retry is a new
//! task over the work still owed, and whatever ran on a machine stays on it.
//!
//! Four guarantees decide the structure:
//!
//! * **Nothing happens before confirmation.** `BatchRunner::start` takes a
//!   `ConfirmedBatch`, so a preview that was never confirmed cannot open a session.
//! * **Trust is still the user's decision.** An unknown host identity or a
//!   ProxyCommand stops *that host* and asks; the question travels to the caller as
//!   an event and the answer comes back through `BatchRunner::answer_trust`. Nothing
//!   is accepted on the user's behalf, and the other hosts keep going.
//! * **Pause and cancel are not the same as "stopped".** A pause starts no new step
//!   and leaves a running command alone. A cancel asks the remote side to stop, marks
//!   the run `cancelling`, and only records a step as cancelled after the channel has
//!   really closed.
//! * **One host's failure is that host's.** The failed host skips its remaining steps
//!   and the others are untouched (unless the task asked for the stricter policy).
//!
//! The transport is a trait ([`HostTransport`]) so the scheduler can be exercised
//! without a network, and the production implementation (`SshHostTransport`) is the
//! one that reuses the existing direct and chain connections, their trust decisions,
//! their authentication and their resolved connection policy.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use russh::ChannelMsg;
use shellune_core::{
    BatchEvent, BatchEventKind, BatchTarget, ConfirmedBatch, FailurePolicy, HostId, HostState,
    OutputStream, RunState, StepState,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::chain::{ChainEvent, ChainRequest, ChainSession, HopAuth};
use crate::error::ChainFailure;
use crate::hostkey::KnownHostKey;
use crate::session::{ConnectRequest, PtySize, RemoteEvent, RemoteSession};
use crate::sftp::ChannelFailure;

/// How often a blocking wait looks at the cancel flag again.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// How often the SSH transport polls its session while a command runs.
const SESSION_POLL: Duration = Duration::from_millis(25);

/// The largest chunk one output event carries.
///
/// Output is written line by line to the run log, so a command that prints a gigabyte
/// must not become one event; the chunk bound is what keeps a single log line small.
pub const OUTPUT_CHUNK_BYTES: usize = 4 * 1024;

/// A flag a transport can wait on, set when the user cancels the run.
///
/// It is shared rather than polled by the transport's own loop so a cancel reaches a
/// command that is already running: the transport closes the channel only because
/// this value changed.
#[derive(Clone, Debug)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl CancelToken {
    /// A token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Whether a cancel was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Asks every waiter to stop.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Finishes when a cancel is requested, for a transport that drives a connection
    /// with `tokio::select!`.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Why one step did not succeed.
///
/// It is separate from a plain message because the two cases are treated very
/// differently: a failure skips the host, while a cancellation ends the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepFailure {
    /// The command ran and failed, or the connection to that host failed.
    Failed(String),
    /// The user cancelled; the remote command has ended.
    Cancelled,
}

/// What a question about one connection means (R19).
///
/// Both variants are decisions only the user may make; a batch run asks instead of
/// accepting, so a task aimed at a group of servers cannot quietly trust a new key
/// on one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustQuestion {
    /// The server presented an identity that is not the trusted one.
    HostKey {
        /// The host name, for the question the caller shows.
        label: String,
        /// The address that presented the key.
        host: String,
        /// The port that presented the key.
        port: u16,
        /// The key algorithm.
        algorithm: String,
        /// The SHA-256 fingerprint to compare.
        fingerprint: String,
    },
    /// A ProxyCommand would run on this machine and is waiting for confirmation.
    ProxyCommand {
        /// The host name, for the question the caller shows.
        label: String,
        /// The command text that would run locally.
        command: String,
    },
}

/// What one transport can do with one host.
///
/// Implementations own their own connection. The runner calls them on a worker
/// thread, one host at a time, so an implementation may block.
pub trait HostTransport: Send {
    /// Opens the connection and stops when it is ready for commands.
    ///
    /// A trust question is asked through `control` and the call returns only after the
    /// answer, because the connection cannot continue before it. The host's connection
    /// policy is the one frozen in the target, not a value read now.
    ///
    /// # Errors
    ///
    /// Returns the reason this host cannot run, which becomes its failure detail.
    fn connect(&mut self, control: &HostControl) -> Result<(), String>;

    /// Runs one command and streams its output to `output`.
    ///
    /// The call returns when the remote command has really ended: `Ok(exit)` with the
    /// status the server reported, or `Err(StepFailure::Cancelled)` after a cancel
    /// request has closed the channel and the end of the channel was observed.
    ///
    /// # Errors
    ///
    /// Returns `StepFailure::Failed` for a command that could not be started, and
    /// `StepFailure::Cancelled` for a request that was honoured.
    fn exec(
        &mut self,
        command: &str,
        control: &HostControl,
        output: &mut dyn FnMut(OutputStream, &[u8]),
    ) -> Result<Option<u32>, StepFailure>;

    /// Ends the connection, whatever state it is in.
    fn disconnect(&mut self);
}

/// How the runner reports and asks while it drives one host.
///
/// The worker owns this value for the length of one host; it is how a transport
/// reaches the run's log, the cancel flag and the user's decisions.
#[derive(Clone)]
pub struct HostControl {
    host: HostId,
    control: Arc<Control>,
}

impl HostControl {
    /// The host this control belongs to.
    #[must_use]
    pub fn host(&self) -> HostId {
        self.host
    }

    /// The cancel flag of the run.
    #[must_use]
    pub fn cancel_token(&self) -> CancelToken {
        self.control.cancel.clone()
    }

    /// Whether a cancel was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.control.cancel.is_cancelled()
    }

    /// Asks the user a question that only they may answer, and waits for the answer.
    ///
    /// The question is written to the run log before the call blocks, which is what
    /// lets the window show it. `None` means the run was cancelled or ended while the
    /// question was still open; a transport must treat that as "do not proceed".
    pub fn ask(&self, question: TrustQuestion) -> Option<bool> {
        let pending = match &question {
            TrustQuestion::HostKey {
                label,
                algorithm,
                fingerprint,
                ..
            } => BatchEventKind::TrustPending {
                host: self.host,
                label: label.clone(),
                algorithm: algorithm.clone(),
                fingerprint: fingerprint.clone(),
            },
            TrustQuestion::ProxyCommand { label, command } => BatchEventKind::ProxyPending {
                host: self.host,
                command: format!("{label}: {command}"),
            },
        };
        self.control.emit(pending);
        let answer = self
            .control
            .trust
            .ask(self.host, question, &self.control.cancel);
        self.control.emit(BatchEventKind::TrustDecided {
            host: self.host,
            accepted: answer == Some(true),
        });
        answer
    }
}

/// Where the runner gets a transport for one target.
///
/// The factory is what knows how a target is reached; the runner only knows the
/// order, the limit and the states. A factory that cannot resolve a target reports
/// why, and that host fails without touching the others.
pub trait TransportFactory: Send + Sync {
    /// A transport for one target, ready to connect.
    ///
    /// # Errors
    ///
    /// Returns the reason this target cannot be attempted at all, such as a missing
    /// credential for that host.
    fn open(&self, target: &BatchTarget) -> Result<Box<dyn HostTransport>, String>;
}

/// How one target is dialled: a direct connection, or the saved jump chain.
pub enum HostRoute {
    /// One direct connection to a saved host.
    Direct {
        /// The saved configuration the snapshot was made from.
        host: Box<shellune_core::HostConfig>,
        /// The one method this attempt uses.
        auth: shellune_core::AuthMethod,
        /// What the trust store said before the attempt.
        known: KnownHostKey,
    },
    /// A chain through saved jump hosts to the target.
    Chain {
        /// The saved hosts the chain was resolved from.
        hosts: Vec<shellune_core::HostConfig>,
        /// The target host id.
        target: HostId,
        /// One authentication method and trust record per hop.
        auth: Vec<HopAuth>,
    },
}

impl HostRoute {
    /// The host this route reaches.
    #[must_use]
    pub fn host_id(&self) -> HostId {
        match self {
            Self::Direct { host, .. } => host.id(),
            Self::Chain { target, .. } => *target,
        }
    }
}

/// Where a run's credentials and saved configuration come from.
///
/// A route is taken once: `AuthMethod` is deliberately not cloneable, so a password
/// or a decrypted key lives only as long as the one attempt that uses it.
pub trait RouteSource: Send + Sync {
    /// The route for one target, consumed by the call.
    ///
    /// # Errors
    ///
    /// Returns why this target cannot be dialled, such as a missing credential.
    fn take_route(&self, target: &BatchTarget) -> Result<HostRoute, String>;
}

/// The routes of one run, keyed by host.
///
/// The caller builds this while the batch is still a preview, so everything a run
/// needs is in memory before the first session exists.
#[derive(Default)]
pub struct SshRoutes {
    routes: Mutex<HashMap<HostId, HostRoute>>,
}

impl SshRoutes {
    /// An empty set of routes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the route for one host, replacing a route for the same host.
    pub fn insert(&self, route: HostRoute) {
        let host = route.host_id();
        if let Ok(mut routes) = self.routes.lock() {
            routes.insert(host, route);
        }
    }

    /// Whether a route was added for this host.
    #[must_use]
    pub fn has(&self, host: HostId) -> bool {
        self.routes
            .lock()
            .map(|routes| routes.contains_key(&host))
            .unwrap_or(false)
    }
}

impl RouteSource for SshRoutes {
    fn take_route(&self, target: &BatchTarget) -> Result<HostRoute, String> {
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| "the routes could not be read".to_owned())?;
        routes.remove(&target.host_id()).ok_or_else(|| {
            format!(
                "{} has no saved connection for this run; connect to it once and try again",
                target.label()
            )
        })
    }
}

/// The production factory: a transport that dials with the saved SSH layer.
pub struct SshTransports {
    routes: Arc<dyn RouteSource>,
}

impl SshTransports {
    /// A factory that takes its routes from `routes`.
    #[must_use]
    pub fn new(routes: Arc<dyn RouteSource>) -> Self {
        Self { routes }
    }
}

impl TransportFactory for SshTransports {
    fn open(&self, target: &BatchTarget) -> Result<Box<dyn HostTransport>, String> {
        Ok(Box::new(SshHostTransport::new(
            target.clone(),
            Arc::clone(&self.routes),
        )))
    }
}

/// How a run bounds its work.
#[derive(Clone, Copy, Debug)]
pub struct RunnerOptions {
    /// How many hosts may have a connection open at the same time.
    ///
    /// It is the task's own concurrency unless the caller lowers it, so the promise in
    /// the confirmation dialog is the promise the run keeps.
    pub concurrency: u32,
}

impl RunnerOptions {
    /// The options that follow the task exactly.
    #[must_use]
    pub fn from_task(confirmed: &ConfirmedBatch) -> Self {
        Self {
            concurrency: confirmed.task().concurrency(),
        }
    }
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self { concurrency: 4 }
    }
}

/// One run of one confirmed batch.
///
/// The runner starts the workers immediately; everything it learns arrives as events,
/// which is the same sequence the log holds. It never blocks the caller.
pub struct BatchRunner {
    control: Arc<Control>,
    events: UnboundedReceiver<BatchEvent>,
    coordinator: Option<std::thread::JoinHandle<()>>,
}

impl BatchRunner {
    /// Starts one run of a confirmed batch.
    ///
    /// This is the only place a batch opens a connection: the confirmed value is the
    /// proof that the user saw the targets, the steps, the concurrency and the failure
    /// policy first.
    ///
    /// # Panics
    ///
    /// Panics when a worker thread cannot be created, because a run that cannot start
    /// must not look like one that started.
    #[must_use]
    pub fn start(
        confirmed: ConfirmedBatch,
        factory: Arc<dyn TransportFactory>,
        options: RunnerOptions,
    ) -> Self {
        let (sender, events) = unbounded_channel();
        let concurrency = confirmed
            .task()
            .concurrency()
            .min(options.concurrency.max(1));
        let control = Arc::new(Control::new(
            sender,
            concurrency,
            confirmed.task().on_failure(),
        ));
        let task = confirmed.task().clone();
        control.emit(BatchEventKind::RunStarted {
            hosts: task.targets().len(),
            steps: task.steps().len(),
            concurrency,
        });

        let mut workers = Vec::new();
        for target in task.targets() {
            let worker = Arc::clone(&control);
            let factory = Arc::clone(&factory);
            let target = target.clone();
            let steps: Vec<String> = task
                .steps()
                .iter()
                .map(|step| step.command().to_owned())
                .collect();
            let handle = std::thread::Builder::new()
                .name(format!("shellune-batch-{}", target.label()))
                .spawn(move || run_host(worker, factory, target, steps))
                .expect("the batch worker thread can be created");
            workers.push(handle);
        }

        let coordinator_control = Arc::clone(&control);
        let coordinator = std::thread::Builder::new()
            .name("shellune-batch-run".to_owned())
            .spawn(move || {
                for worker in workers {
                    let _ = worker.join();
                }
                coordinator_control.finish();
            })
            .expect("the batch coordinator thread can be created");

        Self {
            control,
            events,
            coordinator: Some(coordinator),
        }
    }

    /// Stops starting new steps and leaves a running command alone.
    ///
    /// Hosts that are already connected keep their connection; the pause is about not
    /// starting the next step, which is exactly what R19 promises.
    pub fn pause(&self) {
        if self.control.is_finished() {
            return;
        }
        self.control.scheduler.set(SchedState::Paused);
        self.control.emit(BatchEventKind::RunState {
            state: RunState::Paused,
        });
    }

    /// Continues a paused run from the steps that were left.
    pub fn resume(&self) {
        if self.control.is_finished() {
            return;
        }
        self.control.scheduler.set(SchedState::Running);
        self.control.emit(BatchEventKind::RunState {
            state: RunState::Running,
        });
    }

    /// Asks the run to stop.
    ///
    /// The run is reported as `cancelling` at once and as `cancelled` only after every
    /// running command has ended; no new step starts in between.
    pub fn cancel(&self) {
        if self.control.is_finished() {
            return;
        }
        self.control.scheduler.set(SchedState::Cancelling);
        self.control.cancel.cancel();
        self.control.emit(BatchEventKind::RunState {
            state: RunState::Cancelling,
        });
    }

    /// Answers the open question for one host.
    ///
    /// Returns false when that host was not asking, so a caller that stores a trust
    /// record can tell an answer that was used from one that was not.
    #[must_use]
    pub fn answer_trust(&self, host: HostId, accept: bool) -> bool {
        self.control.trust.answer(host, accept)
    }

    /// The next event, when one has arrived.
    pub fn try_event(&mut self) -> Option<BatchEvent> {
        self.events.try_recv().ok()
    }

    /// Every event that has arrived, in order.
    pub fn drain(&mut self) -> Vec<BatchEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Whether every worker and the coordinator have finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.control.is_finished()
    }

    /// How many hosts this run may have connected at once.
    ///
    /// It is the effective value after the task's own limit and the caller's option
    /// were applied, which is the number a running page can show.
    #[must_use]
    pub fn concurrency(&self) -> u32 {
        self.control.concurrency
    }

    /// Waits for the run to end and returns everything it logged.
    ///
    /// # Panics
    ///
    /// Panics when the coordinator thread panicked, because the run then has no
    /// trustworthy final state.
    pub fn join(mut self) -> Vec<BatchEvent> {
        if let Some(coordinator) = self.coordinator.take() {
            coordinator.join().expect("the batch coordinator finished");
        }
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }
}

/// How far the scheduler lets a worker go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SchedState {
    Running,
    Paused,
    Cancelling,
    Stopped,
}

/// Whether a worker may start the next step.
enum Turn {
    Go,
    Stop,
}

/// The pause gate every worker checks before starting a step.
struct Scheduler {
    state: Mutex<SchedState>,
    wake: Condvar,
}

impl Scheduler {
    fn new() -> Self {
        Self {
            state: Mutex::new(SchedState::Running),
            wake: Condvar::new(),
        }
    }

    fn wait_turn(&self) -> Turn {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        loop {
            match *state {
                SchedState::Running => return Turn::Go,
                SchedState::Paused => {
                    state = match self.wake.wait(state) {
                        Ok(state) => state,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                }
                SchedState::Cancelling | SchedState::Stopped => return Turn::Stop,
            }
        }
    }

    fn set(&self, next: SchedState) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        *state = next;
        drop(state);
        self.wake.notify_all();
    }
}

/// The open host-key and ProxyCommand questions of a run.
#[derive(Default)]
struct TrustState {
    pending: BTreeMap<HostId, TrustQuestion>,
    answers: BTreeMap<HostId, bool>,
}

struct TrustGate {
    state: Mutex<TrustState>,
    wake: Condvar,
}

impl TrustGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(TrustState::default()),
            wake: Condvar::new(),
        }
    }

    fn ask(&self, host: HostId, question: TrustQuestion, cancel: &CancelToken) -> Option<bool> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if cancel.is_cancelled() {
            return None;
        }
        state.pending.insert(host, question);
        loop {
            if let Some(answer) = state.answers.remove(&host) {
                state.pending.remove(&host);
                return Some(answer);
            }
            if cancel.is_cancelled() {
                state.pending.remove(&host);
                return None;
            }
            let (next, _) = match self.wake.wait_timeout(state, CANCEL_POLL) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            state = next;
        }
    }

    fn answer(&self, host: HostId, accept: bool) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !state.pending.contains_key(&host) {
            return false;
        }
        state.answers.insert(host, accept);
        drop(state);
        self.wake.notify_all();
        true
    }
}

/// The connection slots a run shares between its hosts.
struct Permits {
    free: Mutex<usize>,
    wake: Condvar,
}

impl Permits {
    fn new(slots: usize) -> Self {
        Self {
            free: Mutex::new(slots),
            wake: Condvar::new(),
        }
    }

    fn acquire(&self, cancel: &CancelToken) -> bool {
        let mut free = match self.free.lock() {
            Ok(free) => free,
            Err(poisoned) => poisoned.into_inner(),
        };
        while *free == 0 {
            if cancel.is_cancelled() {
                return false;
            }
            let (next, _) = match self.wake.wait_timeout(free, CANCEL_POLL) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            free = next;
        }
        if cancel.is_cancelled() {
            return false;
        }
        *free -= 1;
        true
    }

    fn release(&self) {
        let mut free = match self.free.lock() {
            Ok(free) => free,
            Err(poisoned) => poisoned.into_inner(),
        };
        *free += 1;
        drop(free);
        self.wake.notify_one();
    }
}

/// Everything the workers of one run share.
struct Control {
    events: UnboundedSender<BatchEvent>,
    seq: AtomicU64,
    scheduler: Scheduler,
    cancel: CancelToken,
    trust: TrustGate,
    permits: Permits,
    concurrency: u32,
    on_failure: FailurePolicy,
    any_failed: AtomicBool,
    any_stopped: AtomicBool,
    stop_scheduling: AtomicBool,
    finished: AtomicBool,
}

impl Control {
    fn new(
        events: UnboundedSender<BatchEvent>,
        concurrency: u32,
        on_failure: FailurePolicy,
    ) -> Self {
        Self {
            events,
            seq: AtomicU64::new(0),
            scheduler: Scheduler::new(),
            cancel: CancelToken::new(),
            trust: TrustGate::new(),
            permits: Permits::new(usize::try_from(concurrency).unwrap_or(1).max(1)),
            concurrency: concurrency.max(1),
            on_failure,
            any_failed: AtomicBool::new(false),
            any_stopped: AtomicBool::new(false),
            stop_scheduling: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        }
    }

    fn emit(&self, kind: BatchEventKind) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let _ = self.events.send(BatchEvent::new(seq, now_unix(), kind));
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    fn stop_scheduling(&self) {
        self.stop_scheduling.store(true, Ordering::SeqCst);
    }

    fn should_stop_scheduling(&self) -> bool {
        self.stop_scheduling.load(Ordering::SeqCst) || self.cancel.is_cancelled()
    }

    /// Ends the run: records the state the hosts earned and closes the run.
    fn finish(&self) {
        self.scheduler.set(SchedState::Stopped);
        let state = if self.cancel.is_cancelled() {
            RunState::Cancelled
        } else if self.any_failed.load(Ordering::SeqCst) || self.any_stopped.load(Ordering::SeqCst)
        {
            RunState::Failed
        } else {
            RunState::Completed
        };
        self.finished.store(true, Ordering::SeqCst);
        self.emit(BatchEventKind::RunState { state });
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Runs one host: open a connection, run its steps in order, close it.
fn run_host(
    control: Arc<Control>,
    factory: Arc<dyn TransportFactory>,
    target: BatchTarget,
    steps: Vec<String>,
) {
    let host = target.host_id();
    let host_control = HostControl {
        host,
        control: Arc::clone(&control),
    };

    // A cancelled or stopped run starts nothing, not even a connection.
    if control.should_stop_scheduling() {
        control.any_stopped.store(true, Ordering::SeqCst);
        finish_host(
            &control,
            host,
            if control.cancel.is_cancelled() {
                HostState::Cancelled
            } else {
                HostState::Skipped
            },
            Some("the run stopped before this host started".to_owned()),
        );
        return;
    }
    if !control.permits.acquire(&control.cancel) {
        control.any_stopped.store(true, Ordering::SeqCst);
        finish_host(
            &control,
            host,
            HostState::Cancelled,
            Some("the run was cancelled while this host waited for a slot".to_owned()),
        );
        return;
    }

    let mut transport = match factory.open(&target) {
        Ok(transport) => transport,
        Err(reason) => {
            control.any_failed.store(true, Ordering::SeqCst);
            finish_host(&control, host, HostState::Failed, Some(reason));
            control.permits.release();
            return;
        }
    };

    control.emit(BatchEventKind::HostState {
        host,
        state: HostState::Connecting,
    });
    if let Err(reason) = transport.connect(&host_control) {
        if control.cancel.is_cancelled() {
            control.any_stopped.store(true, Ordering::SeqCst);
            finish_host(&control, host, HostState::Cancelled, Some(reason));
        } else if control.should_stop_scheduling() {
            control.any_stopped.store(true, Ordering::SeqCst);
            finish_host(&control, host, HostState::Skipped, Some(reason));
        } else {
            control.any_failed.store(true, Ordering::SeqCst);
            finish_host(&control, host, HostState::Failed, Some(reason));
        }
        transport.disconnect();
        control.permits.release();
        return;
    }

    let start = usize::try_from(target.starts_at()).unwrap_or(0);
    for (index, command) in steps.iter().enumerate().skip(start) {
        if control.should_stop_scheduling() || matches!(control.scheduler.wait_turn(), Turn::Stop) {
            control.any_stopped.store(true, Ordering::SeqCst);
            finish_host(
                &control,
                host,
                if control.cancel.is_cancelled() {
                    HostState::Cancelled
                } else {
                    HostState::Skipped
                },
                Some("the run stopped before the next step started".to_owned()),
            );
            transport.disconnect();
            control.permits.release();
            return;
        }

        let step = index as u32;
        control.emit(BatchEventKind::StepStarted { host, step });
        let mut sink = |stream: OutputStream, bytes: &[u8]| {
            for chunk in bytes.chunks(OUTPUT_CHUNK_BYTES) {
                if chunk.is_empty() {
                    continue;
                }
                control.emit(BatchEventKind::Output {
                    host,
                    step,
                    stream,
                    chunk: String::from_utf8_lossy(chunk).into_owned(),
                });
            }
        };
        match transport.exec(command, &host_control, &mut sink) {
            Ok(Some(0)) => {
                control.emit(BatchEventKind::StepFinished {
                    host,
                    step,
                    state: StepState::Succeeded,
                    exit: Some(0),
                    detail: None,
                });
            }
            Ok(exit) => {
                let detail = match exit {
                    Some(code) => format!("the command exited with status {code}"),
                    None => "the server closed the channel without an exit status".to_owned(),
                };
                control.emit(BatchEventKind::StepFinished {
                    host,
                    step,
                    state: StepState::Failed,
                    exit,
                    detail: Some(detail.clone()),
                });
                fail_host(&control, host, detail);
                transport.disconnect();
                control.permits.release();
                return;
            }
            Err(StepFailure::Failed(reason)) => {
                control.emit(BatchEventKind::StepFinished {
                    host,
                    step,
                    state: StepState::Failed,
                    exit: None,
                    detail: Some(reason.clone()),
                });
                fail_host(&control, host, reason);
                transport.disconnect();
                control.permits.release();
                return;
            }
            Err(StepFailure::Cancelled) => {
                control.emit(BatchEventKind::StepFinished {
                    host,
                    step,
                    state: StepState::Cancelled,
                    exit: None,
                    detail: Some("cancelled by the user".to_owned()),
                });
                control.any_stopped.store(true, Ordering::SeqCst);
                finish_host(
                    &control,
                    host,
                    HostState::Cancelled,
                    Some("cancelled by the user".to_owned()),
                );
                transport.disconnect();
                control.permits.release();
                return;
            }
        }
    }

    finish_host(&control, host, HostState::Succeeded, None);
    transport.disconnect();
    control.permits.release();
}

fn fail_host(control: &Arc<Control>, host: HostId, detail: String) {
    control.any_failed.store(true, Ordering::SeqCst);
    if control.on_failure == FailurePolicy::StopAfterFailure {
        control.stop_scheduling();
    }
    finish_host(control, host, HostState::Failed, Some(detail));
}

fn finish_host(control: &Arc<Control>, host: HostId, state: HostState, detail: Option<String>) {
    control.emit(BatchEventKind::HostFinished {
        host,
        state,
        detail,
    });
}

/// The production transport: one saved host, dialled with the SSH layer the terminal
/// already uses.
///
/// It exists so a batch run cannot grow a second way to reach a server. The direct
/// path is [`RemoteSession`], the jump path is [`ChainSession`]; both are given the
/// policy frozen in the target snapshot (R20), both ask the user about an unknown
/// identity instead of accepting it, and both run a command through the same
/// §open_exec§ channel the SCP copy uses.
struct SshHostTransport {
    target: BatchTarget,
    routes: Arc<dyn RouteSource>,
    live: Option<Live>,
    runtime: Option<tokio::runtime::Runtime>,
}

enum Live {
    Direct(RemoteSession),
    Chain(ChainSession),
}

impl SshHostTransport {
    fn new(target: BatchTarget, routes: Arc<dyn RouteSource>) -> Self {
        Self {
            target,
            routes,
            live: None,
            runtime: None,
        }
    }

    /// Waits until the connection is ready, answering the questions it raises.
    fn wait_connected(&mut self, control: &HostControl) -> Result<(), String> {
        let label = self.target.label().to_owned();
        loop {
            if control.is_cancelled() {
                return Err("cancelled by the user".to_owned());
            }
            match self.live.as_mut() {
                Some(Live::Direct(session)) => {
                    for event in session.drain() {
                        match event {
                            RemoteEvent::HostKeyPrompt(prompt) => {
                                let question = TrustQuestion::HostKey {
                                    label: label.clone(),
                                    host: prompt.host.clone(),
                                    port: prompt.port,
                                    algorithm: prompt.algorithm().to_owned(),
                                    fingerprint: prompt.fingerprint().to_owned(),
                                };
                                let accepted = control.ask(question) == Some(true);
                                if !accepted {
                                    return Err("the server identity was not accepted".to_owned());
                                }
                                if !session.answer_host_key(true) {
                                    return Err(
                                        "the identity question was already closed".to_owned()
                                    );
                                }
                            }
                            RemoteEvent::Connected => return Ok(()),
                            RemoteEvent::Failed(error) => return Err(error.to_string()),
                            RemoteEvent::Data(_)
                            | RemoteEvent::Exited { .. }
                            | RemoteEvent::X11(_) => {}
                        }
                    }
                }
                Some(Live::Chain(session)) => {
                    for event in session.drain() {
                        match event {
                            ChainEvent::HostKeyPrompt { hop, prompt } => {
                                let question = TrustQuestion::HostKey {
                                    label: hop.name.clone(),
                                    host: prompt.host.clone(),
                                    port: prompt.port,
                                    algorithm: prompt.algorithm().to_owned(),
                                    fingerprint: prompt.fingerprint().to_owned(),
                                };
                                let accepted = control.ask(question) == Some(true);
                                if !accepted {
                                    return Err(format!(
                                        "the identity of hop {} was not accepted",
                                        hop.number()
                                    ));
                                }
                                if !session.answer_host_key(hop.host_id, true) {
                                    return Err(
                                        "the identity question was already closed".to_owned()
                                    );
                                }
                            }
                            ChainEvent::ProxyConfirmation { hop, command } => {
                                let question = TrustQuestion::ProxyCommand {
                                    label: hop.name.clone(),
                                    command,
                                };
                                let allowed = control.ask(question) == Some(true);
                                if !allowed {
                                    return Err(format!(
                                        "the ProxyCommand for hop {} was not confirmed",
                                        hop.number()
                                    ));
                                }
                                if !session.confirm_proxy(hop.host_id, true) {
                                    return Err(
                                        "the ProxyCommand question was already closed".to_owned()
                                    );
                                }
                            }
                            ChainEvent::Connected { .. } => return Ok(()),
                            ChainEvent::Failed(failure) => {
                                return Err(chain_failure_text(&failure));
                            }
                            ChainEvent::HopStarted(_)
                            | ChainEvent::HopReady(_)
                            | ChainEvent::Data(_)
                            | ChainEvent::Exited { .. }
                            | ChainEvent::X11 { .. } => {}
                        }
                    }
                }
                None => return Err("the connection was not started".to_owned()),
            }
            std::thread::sleep(SESSION_POLL);
        }
    }

    /// Whether the connection failed while a command was running.
    fn session_failure(&mut self) -> Option<String> {
        match self.live.as_mut()? {
            Live::Direct(session) => session.drain().into_iter().find_map(|event| match event {
                RemoteEvent::Failed(error) => Some(error.to_string()),
                _ => None,
            }),
            Live::Chain(session) => session.drain().into_iter().find_map(|event| match event {
                ChainEvent::Failed(failure) => Some(chain_failure_text(&failure)),
                _ => None,
            }),
        }
    }
}

/// The text of a chain failure, which already names the hop that caused it.
fn chain_failure_text(failure: &ChainFailure) -> String {
    failure.to_string()
}

/// Why a session channel could not be opened.
fn channel_failure_text(failure: &ChannelFailure) -> String {
    match failure {
        ChannelFailure::Unavailable { reason } | ChannelFailure::Lost { reason } => reason.clone(),
    }
}

impl HostTransport for SshHostTransport {
    fn connect(&mut self, control: &HostControl) -> Result<(), String> {
        let route = self.routes.take_route(&self.target)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("the SSH runtime could not be started: {error}"))?;
        let policy = self.target.policy();
        let live = match route {
            HostRoute::Direct { host, auth, known } => {
                let request = ConnectRequest::new(
                    host.address(),
                    host.port().get(),
                    host.user(),
                    auth,
                    PtySize::default(),
                    known,
                )
                .without_shell()
                .with_connection_policy(policy);
                Live::Direct(RemoteSession::connect(request))
            }
            HostRoute::Chain {
                hosts,
                target,
                auth,
            } => {
                let request = ChainRequest::new(hosts, target, auth, PtySize::default())
                    .without_shell()
                    .with_connection_policy(policy);
                Live::Chain(ChainSession::connect(request))
            }
        };
        self.runtime = Some(runtime);
        self.live = Some(live);
        self.wait_connected(control)
    }

    fn exec(
        &mut self,
        command: &str,
        control: &HostControl,
        output: &mut dyn FnMut(OutputStream, &[u8]),
    ) -> Result<Option<u32>, StepFailure> {
        let Some(handle) = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
        else {
            return Err(StepFailure::Failed(
                "the connection was not started".to_owned(),
            ));
        };
        // The channel is taken out of the connection here so the read loop below owns it;
        // the borrow of `self` ends before the loop, which is what lets the loop ask the
        // session whether it failed while a command was running.
        let channel = {
            let Some(live) = self.live.as_mut() else {
                return Err(StepFailure::Failed(
                    "the connection was not started".to_owned(),
                ));
            };
            let open = match live {
                Live::Direct(session) => session.open_exec(command.to_owned()),
                Live::Chain(session) => session.open_exec(command.to_owned()),
            };
            let answer = open.map_err(StepFailure::Failed)?;
            handle
                .block_on(answer)
                .map_err(|_| {
                    StepFailure::Failed(
                        "the connection ended before the command started".to_owned(),
                    )
                })?
                .map_err(|failure| StepFailure::Failed(channel_failure_text(&failure)))?
        };
        let (mut read, write) = channel.split();
        let mut exit = None;
        let mut asked_to_close = false;
        loop {
            if control.is_cancelled() && !asked_to_close {
                // The remote side is asked to stop, and the step stays "cancelling"
                // until the channel really ends: closing it is not the same as the
                // command having stopped.
                asked_to_close = true;
                let _ = handle.block_on(write.close());
            }
            let next =
                handle.block_on(async { tokio::time::timeout(SESSION_POLL, read.wait()).await });
            match next {
                Ok(Some(ChannelMsg::Data { data })) => output(OutputStream::Stdout, &data),
                Ok(Some(ChannelMsg::ExtendedData { data, ext })) => {
                    let stream = if ext == 1 {
                        OutputStream::Stderr
                    } else {
                        OutputStream::Stdout
                    };
                    output(stream, &data);
                }
                Ok(Some(ChannelMsg::ExitStatus { exit_status })) => {
                    exit = Some(exit_status);
                }
                Ok(Some(ChannelMsg::Eof)) => {}
                Ok(Some(ChannelMsg::Close)) | Ok(None) => break,
                Ok(Some(_)) => {}
                Err(_) => {
                    if let Some(reason) = self.session_failure() {
                        if asked_to_close {
                            return Err(StepFailure::Cancelled);
                        }
                        return Err(StepFailure::Failed(reason));
                    }
                }
            }
        }
        if asked_to_close {
            return Err(StepFailure::Cancelled);
        }
        Ok(exit)
    }

    fn disconnect(&mut self) {
        if let Some(live) = self.live.as_mut() {
            match live {
                Live::Direct(session) => session.close(),
                Live::Chain(session) => session.close(),
            }
        }
        self.live = None;
        self.runtime = None;
    }
}
