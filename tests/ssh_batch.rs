//! Multi-step batch runs against the scheduler and, when this host can start one,
//! against a real OpenSSH server (R19, R21).
//!
//! The scheduling checks drive a transport double: it records which host connected
//! when, how many connections were open at once, and which command ran. That is the
//! only way to check the promises the feature makes about pause, cancel, concurrency
//! and skipping without waiting on a real network.
//!
//! The check against a real server is skipped, loudly, when this host cannot start a
//! disposable OpenSSH server: a synthetic transport proves the scheduling, never SSH.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use shellune_core::{
    AuthMethod, BatchEvent, BatchEventKind, BatchRun, BatchRunId, BatchStep, BatchTarget,
    BatchTask, ConnectionDefaults, FailurePolicy, HostConfig, HostId, HostState, OutputStream,
    RunState, StepState,
};
use shellune_ssh::batch::{
    BatchRunner, HostControl, HostRoute, HostTransport, RunnerOptions, SshRoutes, SshTransports,
    StepFailure, TransportFactory, TrustQuestion,
};
use shellune_ssh::hostkey::KnownHostKey;
use shellune_ssh::testserver::Harness;
use shellune_storage::BatchStore;

const PATIENCE: Duration = Duration::from_secs(30);

fn host(name: &str) -> HostConfig {
    HostConfig::new(name, "192.0.2.9", 22, "deploy").expect("a valid host")
}

fn target(host: &HostConfig) -> BatchTarget {
    BatchTarget::snapshot(host, ConnectionDefaults::default()).expect("a snapshot")
}

fn task(steps: &[&str], hosts: &[&HostConfig], concurrency: u32) -> BatchTask {
    BatchTask::new(
        steps
            .iter()
            .map(|command| BatchStep::new(*command).expect("a valid step"))
            .collect(),
        hosts.iter().map(|each| target(each)).collect(),
        concurrency,
    )
    .expect("a valid task")
}

fn scratch(name: &str) -> std::path::PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "shellune-batch-test-{}-{}-{name}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// Folds every event into the run, which is what the application's result list does.
///
/// It also asserts the contract the runner owes: every event it emits fits the run it
/// names, so a state the log shows is always a state the summary can hold.
fn fold(run: &mut BatchRun, events: &[BatchEvent]) {
    for event in events {
        run.apply(event)
            .unwrap_or_else(|problem| panic!("the run refused {:?}: {problem}", event.kind()));
    }
}

fn wait_for(
    runner: &mut BatchRunner,
    events: &mut Vec<BatchEvent>,
    predicate: impl Fn(&[BatchEvent]) -> bool,
) -> bool {
    let deadline = Instant::now() + PATIENCE;
    loop {
        events.extend(runner.drain());
        if predicate(events) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn step_started(events: &[BatchEvent], host: HostId, step: u32) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind(),
            BatchEventKind::StepStarted { host: asked, step: asked_step }
                if *asked == host && *asked_step == step
        )
    })
}

fn has_state(events: &[BatchEvent], state: RunState) -> bool {
    events.iter().any(|event| {
        matches!(event.kind(), BatchEventKind::RunState { state: asked } if *asked == state)
    })
}

fn position_of(events: &[BatchEvent], wanted: impl Fn(&BatchEventKind) -> bool) -> Option<usize> {
    events.iter().position(|event| wanted(event.kind()))
}

/// What the transport double does when a host connects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Connect {
    Accept,
    AskKey,
    Refuse,
    /// Panics, the way a transport with a bug does, so the scheduler has to decide
    /// what a worker that never reported an end means.
    Panic,
}

#[derive(Clone)]
struct HostPlan {
    connect: Connect,
    fail: HashSet<String>,
    hold: HashSet<String>,
}

impl HostPlan {
    fn new(connect: Connect) -> Self {
        Self {
            connect,
            fail: HashSet::new(),
            hold: HashSet::new(),
        }
    }
}

/// The shared state of the transport double: the plan per host, and what it saw.
#[derive(Default)]
struct FakeHosts {
    plans: Mutex<HashMap<HostId, HostPlan>>,
    open_failures: Mutex<HashSet<HostId>>,
    opened: AtomicUsize,
    max_open: AtomicUsize,
    connects: Mutex<Vec<HostId>>,
    steps: Mutex<Vec<(HostId, String)>>,
    blocked: Mutex<HashSet<String>>,
    gate: Condvar,
}

impl FakeHosts {
    fn plan(&self, host: HostId) -> HostPlan {
        self.plans
            .lock()
            .expect("the plans are readable")
            .get(&host)
            .cloned()
            .unwrap_or_else(|| HostPlan::new(Connect::Accept))
    }

    fn set_plan(&self, host: HostId, plan: HostPlan) {
        self.plans
            .lock()
            .expect("the plans are writable")
            .insert(host, plan);
    }

    fn block(&self, command: &str) {
        self.blocked
            .lock()
            .expect("the block list is writable")
            .insert(command.to_owned());
    }

    fn release(&self, command: &str) {
        self.blocked
            .lock()
            .expect("the block list is writable")
            .remove(command);
        self.gate.notify_all();
    }

    fn is_blocked(&self, command: &str) -> bool {
        self.blocked
            .lock()
            .expect("the block list is readable")
            .contains(command)
    }

    fn record_step(&self, host: HostId, command: &str) {
        self.steps
            .lock()
            .expect("the step list is writable")
            .push((host, command.to_owned()));
    }

    fn steps_of(&self, host: HostId) -> Vec<String> {
        self.steps
            .lock()
            .expect("the step list is readable")
            .iter()
            .filter(|(asked, _)| *asked == host)
            .map(|(_, command)| command.clone())
            .collect()
    }

    fn connects_of(&self, host: HostId) -> usize {
        self.connects
            .lock()
            .expect("the connect log is readable")
            .iter()
            .filter(|asked| **asked == host)
            .count()
    }

    fn wait_release(&self, command: &str, control: &HostControl) -> Result<(), StepFailure> {
        let mut blocked = self.blocked.lock().expect("the block list is readable");
        while blocked.contains(command) {
            if control.is_cancelled() {
                return Err(StepFailure::Cancelled);
            }
            let (next, _) = self
                .gate
                .wait_timeout(blocked, Duration::from_millis(20))
                .expect("the gate is not poisoned");
            blocked = next;
        }
        Ok(())
    }

    fn wait_for_cancel(&self, control: &HostControl) {
        while !control.is_cancelled() {
            std::thread::sleep(Duration::from_millis(10));
        }
        // A real channel closes a moment after it was asked to; the runner must not
        // call the step cancelled before that end is observed.
        std::thread::sleep(Duration::from_millis(60));
    }
}

struct FakeFactory {
    hosts: Arc<FakeHosts>,
}

impl TransportFactory for FakeFactory {
    fn open(&self, target: &BatchTarget) -> Result<Box<dyn HostTransport>, String> {
        if self
            .hosts
            .open_failures
            .lock()
            .expect("the open failures are readable")
            .contains(&target.host_id())
        {
            return Err("the saved connection for this host was removed".to_owned());
        }
        Ok(Box::new(FakeTransport {
            host: target.host_id(),
            label: target.label().to_owned(),
            hosts: Arc::clone(&self.hosts),
            open: false,
        }))
    }
}

struct FakeTransport {
    host: HostId,
    label: String,
    hosts: Arc<FakeHosts>,
    /// Whether this transport currently holds a connection slot.
    open: bool,
}

impl HostTransport for FakeTransport {
    fn connect(&mut self, control: &HostControl) -> Result<(), String> {
        let open = self.hosts.opened.fetch_add(1, Ordering::SeqCst) + 1;
        self.open = true;
        self.hosts.max_open.fetch_max(open, Ordering::SeqCst);
        self.hosts
            .connects
            .lock()
            .expect("the connect log is writable")
            .push(self.host);
        match self.hosts.plan(self.host).connect {
            Connect::Accept => Ok(()),
            Connect::AskKey => {
                let answer = control.ask(TrustQuestion::HostKey {
                    label: self.label.clone(),
                    host: "192.0.2.9".to_owned(),
                    port: 22,
                    algorithm: "ssh-ed25519".to_owned(),
                    fingerprint: "SHA256:the-presented-key".to_owned(),
                });
                if answer != Some(true) {
                    return Err("the server identity was not accepted".to_owned());
                }
                Ok(())
            }
            Connect::Refuse => Err("the connection was refused".to_owned()),
            Connect::Panic => panic!("this transport panics on purpose"),
        }
    }

    fn exec(
        &mut self,
        command: &str,
        control: &HostControl,
        output: &mut dyn FnMut(OutputStream, &[u8]),
    ) -> Result<Option<u32>, StepFailure> {
        self.hosts.record_step(self.host, command);
        let plan = self.hosts.plan(self.host);
        if self.hosts.is_blocked(command) {
            self.hosts.wait_release(command, control)?;
        }
        if plan.hold.contains(command) {
            self.hosts.wait_for_cancel(control);
            return Err(StepFailure::Cancelled);
        }
        output(
            OutputStream::Stdout,
            format!("{command} on {}\n", self.label).as_bytes(),
        );
        if plan.fail.contains(command) {
            output(OutputStream::Stderr, b"the command failed\n");
            return Ok(Some(7));
        }
        Ok(Some(0))
    }

    fn disconnect(&mut self) {
        if self.open {
            self.open = false;
            self.hosts.opened.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn factory(hosts: &Arc<FakeHosts>) -> Arc<dyn TransportFactory> {
    Arc::new(FakeFactory {
        hosts: Arc::clone(hosts),
    })
}

fn runner(task: BatchTask, hosts: &Arc<FakeHosts>, concurrency: u32) -> (BatchRunner, BatchRun) {
    let confirmed = task.confirm(1_700_000_000).expect("a confirmable task");
    let run = BatchRun::planned(&confirmed, BatchRunId::new(), 1_700_000_001);
    let runner = BatchRunner::start(
        confirmed,
        factory(hosts),
        RunnerOptions {
            concurrency: concurrency.max(1),
        },
    );
    (runner, run)
}

#[test]
fn a_task_that_is_not_confirmed_opens_no_session() {
    let one = host("alpha");
    let hosts = Arc::new(FakeHosts::default());
    let preview = task(&["uptime"], &[&one], 2);
    assert_eq!(preview.targets().len(), 1);
    // Building, previewing and even building the routes touches nothing.
    let routes = SshRoutes::new();
    assert!(!routes.has(one.id()));
    assert_eq!(hosts.connects_of(one.id()), 0);
    assert_eq!(hosts.opened.load(Ordering::SeqCst), 0);
    let confirmed = preview.confirm(1_700_000_000).expect("a confirmable task");
    assert_eq!(confirmed.target_count(), 1);
    assert_eq!(
        hosts.connects_of(one.id()),
        0,
        "confirming dials nothing either"
    );
}

#[test]
fn a_successful_task_records_each_step_its_output_and_its_exit() {
    let one = host("alpha");
    let two = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    let (mut runner, mut run) = runner(task(&["uptime", "df -h"], &[&one, &two], 2), &hosts, 2);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Completed
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);

    assert_eq!(run.state(), RunState::Completed);
    for host in [one.id(), two.id()] {
        let record = run.host(host).expect("the host is in the run");
        assert_eq!(record.state(), HostState::Succeeded);
        assert_eq!(record.steps()[0].state(), StepState::Succeeded);
        assert_eq!(record.steps()[0].exit(), Some(0));
        assert_eq!(hosts.steps_of(host), vec!["uptime", "df -h"]);
    }
    let output: Vec<String> = events
        .iter()
        .filter_map(|event| match event.kind() {
            BatchEventKind::Output {
                stream: OutputStream::Stdout,
                chunk,
                ..
            } => Some(chunk.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(output.len(), 4, "one line per step on each host");
    assert!(output.iter().any(|line| line.contains("uptime on alpha")));
    assert!(
        position_of(&events, |kind| matches!(
            kind,
            BatchEventKind::StepFinished { step: 0, .. }
        ))
        .is_some(),
        "the first step's end is recorded"
    );
    assert!(
        hosts.max_open.load(Ordering::SeqCst) <= 2,
        "two hosts, two slots"
    );
}

#[test]
fn a_failed_host_skips_its_remaining_steps_and_the_others_finish() {
    let one = host("alpha");
    let two = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    let mut plan = HostPlan::new(Connect::Accept);
    plan.fail.insert("apply".to_owned());
    hosts.set_plan(one.id(), plan);

    let (mut runner, mut run) = runner(
        task(&["check", "apply", "restart"], &[&one, &two], 2),
        &hosts,
        2,
    );
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Failed
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);

    assert_eq!(run.state(), RunState::Failed);
    let failed = run.host(one.id()).expect("the failed host");
    assert_eq!(failed.state(), HostState::Failed);
    assert_eq!(failed.steps()[1].state(), StepState::Failed);
    assert_eq!(failed.steps()[1].exit(), Some(7));
    assert_eq!(failed.steps()[2].state(), StepState::Skipped);
    assert_eq!(hosts.steps_of(one.id()), vec!["check", "apply"]);
    let healthy = run.host(two.id()).expect("the healthy host");
    assert_eq!(healthy.state(), HostState::Succeeded);
    assert_eq!(hosts.steps_of(two.id()), vec!["check", "apply", "restart"]);
    // The failure is written down with the exit code and the output around it.
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        BatchEventKind::Output {
            stream: OutputStream::Stderr,
            ..
        }
    )));
}

#[test]
fn a_connection_that_cannot_be_opened_or_is_refused_fails_only_that_host() {
    let one = host("alpha");
    let refused = host("beta");
    let two = host("gamma");
    let hosts = Arc::new(FakeHosts::default());
    hosts
        .open_failures
        .lock()
        .expect("the open failures are writable")
        .insert(one.id());
    hosts.set_plan(refused.id(), HostPlan::new(Connect::Refuse));

    let (mut runner, mut run) = runner(task(&["uptime"], &[&one, &refused, &two], 3), &hosts, 3);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Failed
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    let failed = run.host(one.id()).expect("the failed host");
    assert_eq!(failed.state(), HostState::Failed);
    assert!(failed.detail().expect("a reason").contains("removed"));
    let refused_record = run.host(refused.id()).expect("the refused host");
    assert_eq!(refused_record.state(), HostState::Failed);
    assert!(
        refused_record
            .detail()
            .expect("a reason")
            .contains("refused")
    );
    assert_eq!(
        run.host(two.id()).expect("the other").state(),
        HostState::Succeeded
    );
}

#[test]
fn a_retry_runs_only_the_host_and_steps_that_did_not_finish() {
    let one = host("alpha");
    let two = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    let mut plan = HostPlan::new(Connect::Accept);
    plan.fail.insert("apply".to_owned());
    hosts.set_plan(one.id(), plan);

    let (mut runner, mut run) = runner(
        task(&["check", "apply", "restart"], &[&one, &two], 2),
        &hosts,
        2,
    );
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Failed
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    assert_eq!(
        run.host(one.id()).expect("the host").steps()[1].state(),
        StepState::Failed
    );

    // The user fixes the host and retries: only alpha, and only from the step it failed
    // at, so its successful "check" is not run again and beta is not touched.
    let connects_before = hosts.connects_of(two.id());
    hosts.set_plan(one.id(), HostPlan::new(Connect::Accept));
    let retry = BatchTask::retry_after(&run).expect("a retry task");
    assert_eq!(retry.targets().len(), 1);
    assert_eq!(retry.targets()[0].starts_at(), 1);
    let confirmed = retry.clone().confirm(1_700_000_100).expect("confirmable");
    let mut retry_run = BatchRun::planned(&confirmed, BatchRunId::new(), 1_700_000_101);
    let mut retry_runner =
        BatchRunner::start(confirmed, factory(&hosts), RunnerOptions { concurrency: 2 });
    let mut retry_events = Vec::new();
    assert!(wait_for(&mut retry_runner, &mut retry_events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState { state } if state.is_finished()
            )
        })
    }));
    retry_events.extend(retry_runner.join());
    fold(&mut retry_run, &retry_events);

    assert_eq!(retry_run.state(), RunState::Completed);
    assert_eq!(
        retry_run.host(one.id()).expect("the host").steps()[0].state(),
        StepState::AlreadyDone
    );
    assert_eq!(
        hosts.steps_of(one.id()),
        vec!["check", "apply", "apply", "restart"],
        "the retry ran the failed step and the one after it, not the one that succeeded"
    );
    assert_eq!(
        hosts.connects_of(two.id()),
        connects_before,
        "the host that succeeded was not connected again"
    );
}

#[test]
fn the_run_never_has_more_connections_open_than_the_limit() {
    let hosts: Vec<HostConfig> = (0..4).map(|index| host(&format!("host{index}"))).collect();
    let fake = Arc::new(FakeHosts::default());
    fake.block("block");
    let (mut runner, mut run) = runner(
        task(&["block", "uptime"], &hosts.iter().collect::<Vec<_>>(), 2),
        &fake,
        2,
    );
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events
            .iter()
            .filter(|event| matches!(event.kind(), BatchEventKind::StepStarted { step: 0, .. }))
            .count()
            >= 2
    }));
    // Two are running their first step and the other two must be waiting for a slot.
    assert_eq!(fake.opened.load(Ordering::SeqCst), 2);
    fake.release("block");
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState { state } if state.is_finished()
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    assert_eq!(run.state(), RunState::Completed);
    assert!(fake.max_open.load(Ordering::SeqCst) <= 2);
}

#[test]
fn a_pause_starts_no_new_step_until_the_run_is_resumed() {
    let one = host("alpha");
    let hosts = Arc::new(FakeHosts::default());
    hosts.block("first");
    let (mut runner, mut run) = runner(task(&["first", "second", "third"], &[&one], 1), &hosts, 1);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        step_started(events, one.id(), 0)
    }));
    runner.pause();
    hosts.release("first");
    // The step that was already running finishes; the next one must not start.
    assert!(wait_for(&mut runner, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind(), BatchEventKind::StepFinished { step: 0, .. }))
    }));
    std::thread::sleep(Duration::from_millis(250));
    events.extend(runner.drain());
    assert!(
        !step_started(&events, one.id(), 1),
        "a paused run starts nothing"
    );
    assert!(has_state(&events, RunState::Paused));

    runner.resume();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Completed
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    assert_eq!(run.state(), RunState::Completed);
    assert_eq!(hosts.steps_of(one.id()), vec!["first", "second", "third"]);
}

#[test]
fn a_cancel_is_reported_as_cancelling_until_the_running_command_ends() {
    let one = host("alpha");
    let hosts = Arc::new(FakeHosts::default());
    let mut plan = HostPlan::new(Connect::Accept);
    plan.hold.insert("long".to_owned());
    hosts.set_plan(one.id(), plan);
    let (mut runner, mut run) = runner(task(&["long", "never"], &[&one], 1), &hosts, 1);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        step_started(events, one.id(), 0)
    }));
    runner.cancel();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Cancelled
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);

    let cancelling = position_of(&events, |kind| {
        matches!(
            kind,
            BatchEventKind::RunState {
                state: RunState::Cancelling
            }
        )
    })
    .expect("the run said it was cancelling");
    let ended = position_of(&events, |kind| {
        matches!(
            kind,
            BatchEventKind::StepFinished {
                step: 0,
                state: StepState::Cancelled,
                ..
            }
        )
    })
    .expect("the step really ended");
    assert!(
        cancelling < ended,
        "cancelling is reported before the step is called cancelled"
    );
    assert!(
        !step_started(&events, one.id(), 1),
        "no step starts after a cancel"
    );
    assert_eq!(hosts.steps_of(one.id()), vec!["long"]);
    assert_eq!(run.state(), RunState::Cancelled);
    assert_eq!(
        run.host(one.id()).expect("the host").state(),
        HostState::Cancelled
    );
}

#[test]
fn an_unknown_identity_waits_for_the_user_and_only_that_host_waits() {
    let asking = host("alpha");
    let other = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    hosts.set_plan(asking.id(), HostPlan::new(Connect::AskKey));

    let (mut runner, mut run) = runner(task(&["uptime"], &[&asking, &other], 2), &hosts, 2);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind(), BatchEventKind::TrustPending { .. }))
    }));
    // The other host must not be held up by a question that is not about it.
    assert!(wait_for(&mut runner, &mut events, |events| {
        step_started(events, other.id(), 0)
    }));
    // No trust record was written on the user's behalf: the answer is a real decision.
    assert!(runner.answer_trust(asking.id(), false));
    assert!(
        !runner.answer_trust(other.id(), true),
        "that host is not asking"
    );
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState { state } if state.is_finished()
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    let refused = run.host(asking.id()).expect("the asking host");
    assert_eq!(refused.state(), HostState::Failed);
    assert!(refused.detail().expect("a reason").contains("not accepted"));
    assert_eq!(hosts.steps_of(asking.id()), Vec::<String>::new());
    assert_eq!(
        run.host(other.id()).expect("the other host").state(),
        HostState::Succeeded
    );
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        BatchEventKind::TrustDecided {
            accepted: false,
            ..
        }
    )));
}

#[test]
fn the_stricter_failure_policy_stops_starting_new_steps() {
    let one = host("alpha");
    let two = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    let mut plan = HostPlan::new(Connect::Accept);
    plan.fail.insert("check".to_owned());
    hosts.set_plan(one.id(), plan);
    hosts.block("slow");

    let mut task = task(&["slow", "check"], &[&one, &two], 1);
    task.set_failure_policy(FailurePolicy::StopAfterFailure);
    let (mut runner, mut run) = runner(task, &hosts, 1);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        step_started(events, one.id(), 0)
    }));
    hosts.release("slow");
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState {
                    state: RunState::Failed
                }
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    assert_eq!(run.state(), RunState::Failed);
    assert_eq!(
        run.host(one.id()).expect("the host").state(),
        HostState::Failed
    );
    let other = run.host(two.id()).expect("the other host");
    assert_eq!(other.state(), HostState::Skipped);
    assert!(
        hosts.steps_of(two.id()).is_empty(),
        "a stopped run starts nothing else"
    );
}

#[test]
fn the_run_log_and_the_run_summary_agree_and_a_restart_marks_the_unfinished_run() {
    let one = host("alpha");
    let two = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    let store = BatchStore::at(scratch("run"));
    let confirmed = task(&["uptime", "df -h"], &[&one, &two], 2)
        .confirm(1_700_000_000)
        .expect("a confirmable task");
    let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 1_700_000_001);
    store.save_run(&run).expect("the planned run saves");
    let mut runner =
        BatchRunner::start(confirmed, factory(&hosts), RunnerOptions { concurrency: 2 });
    let mut persisted = 0;
    while !runner.is_finished() {
        for event in runner.drain() {
            store
                .append(run.id(), event.kind().clone(), event.at_unix())
                .expect("the line is appended");
            run.apply(&event).expect("the event folds");
            persisted += 1;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for event in runner.join() {
        store
            .append(run.id(), event.kind().clone(), event.at_unix())
            .expect("the line is appended");
        run.apply(&event).expect("the event folds");
        persisted += 1;
    }
    store.save_run(&run).expect("the summary saves");

    let log = store.read_events(run.id()).expect("the log reads back");
    assert_eq!(log.events().len(), persisted);
    assert!(!log.tail_ignored());
    let replayed = log
        .replay(BatchRun::planned(
            &run.task().clone().confirm(1).expect("confirmable"),
            run.id(),
            run.started_at_unix(),
        ))
        .expect("the log folds into a run");
    assert_eq!(replayed.state(), RunState::Completed);
    assert_eq!(
        replayed.hosts(),
        run.hosts(),
        "the log and the summary agree"
    );

    // A second run that never finished is marked by the restart, and is not replayed.
    let unfinished = task(&["uptime"], &[&one], 1)
        .confirm(1_700_000_002)
        .expect("a confirmable task");
    let pending = BatchRun::planned(&unfinished, BatchRunId::new(), 1_700_000_003);
    store.save_run(&pending).expect("the pending run saves");
    let recovered = store
        .recover_interrupted(1_700_000_004)
        .expect("the restart runs");
    assert_eq!(recovered, vec![pending.id()]);
    assert_eq!(
        store.load_run(pending.id()).expect("the run loads").state(),
        RunState::Interrupted
    );
    assert_eq!(
        store
            .load_run(run.id())
            .expect("the finished run loads")
            .state(),
        RunState::Completed,
        "a restart does not touch a finished run"
    );
}

#[test]
fn a_pause_or_a_resume_after_a_cancel_writes_nothing() {
    let one = host("alpha");
    let hosts = Arc::new(FakeHosts::default());
    let mut plan = HostPlan::new(Connect::Accept);
    plan.hold.insert("long".to_owned());
    hosts.set_plan(one.id(), plan);
    let (mut runner, mut run) = runner(task(&["long", "never"], &[&one], 1), &hosts, 1);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        step_started(events, one.id(), 0)
    }));
    runner.cancel();
    // A cancel cannot become a pause and a cancel is not a run to resume: both calls
    // must write nothing, because the run model refuses those moves.
    runner.pause();
    runner.resume();
    assert!(wait_for(&mut runner, &mut events, |events| {
        has_state(events, RunState::Cancelled)
    }));
    events.extend(runner.join());
    // Folding refuses a state the run cannot hold, so this is also the check that no
    // refused transition was written.
    fold(&mut run, &events);
    assert!(
        !has_state(&events, RunState::Paused),
        "a cancel is never a pause"
    );
    let cancelling = position_of(&events, |kind| {
        matches!(
            kind,
            BatchEventKind::RunState {
                state: RunState::Cancelling
            }
        )
    })
    .expect("the run said it was cancelling");
    assert!(
        !events[cancelling..].iter().any(|event| matches!(
            event.kind(),
            BatchEventKind::RunState {
                state: RunState::Running
            }
        )),
        "a cancelled run is not pushed back to running"
    );
    assert_eq!(run.state(), RunState::Cancelled);
}

#[test]
fn a_pause_or_a_cancel_after_the_run_finished_writes_nothing() {
    let one = host("alpha");
    let hosts = Arc::new(FakeHosts::default());
    let (mut runner, mut run) = runner(task(&["uptime"], &[&one], 1), &hosts, 1);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        has_state(events, RunState::Completed)
    }));
    // The run is over, so none of these may be written as a state it refused.
    runner.pause();
    runner.resume();
    runner.cancel();
    events.extend(runner.join());
    fold(&mut run, &events);
    assert_eq!(run.state(), RunState::Completed);
    assert!(!has_state(&events, RunState::Paused));
    assert!(!has_state(&events, RunState::Cancelling));
}

#[test]
fn a_question_does_not_hold_the_only_connection_slot() {
    let first = host("alpha");
    let second = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    hosts.set_plan(first.id(), HostPlan::new(Connect::AskKey));
    hosts.set_plan(second.id(), HostPlan::new(Connect::AskKey));
    let (mut runner, mut run) = runner(task(&["uptime"], &[&first, &second], 1), &hosts, 1);
    let asked = |events: &[BatchEvent], host: HostId| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::TrustPending { host: asked, .. } if *asked == host
            )
        })
    };
    let mut events = Vec::new();
    // Both questions arrive before either is answered: a host that is only waiting for
    // the user cannot hold the single connection slot the other host needs to ask.
    assert!(wait_for(&mut runner, &mut events, |events| {
        asked(events, first.id()) && asked(events, second.id())
    }));
    assert_eq!(runner.concurrency(), 1);
    assert!(runner.answer_trust(first.id(), true));
    assert!(runner.answer_trust(second.id(), true));
    assert!(wait_for(&mut runner, &mut events, |events| {
        has_state(events, RunState::Completed)
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    for host in [first.id(), second.id()] {
        assert_eq!(
            run.host(host).expect("the host").state(),
            HostState::Succeeded
        );
    }
}

#[test]
fn a_worker_that_panics_fails_its_host_and_keeps_the_run_from_completing() {
    let broken = host("alpha");
    let other = host("beta");
    let hosts = Arc::new(FakeHosts::default());
    hosts.set_plan(broken.id(), HostPlan::new(Connect::Panic));
    let (mut runner, mut run) = runner(task(&["uptime"], &[&broken, &other], 2), &hosts, 2);
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState { state } if state.is_finished()
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);
    assert_eq!(
        run.state(),
        RunState::Failed,
        "a worker that never reported an end is not a completed run"
    );
    let failed = run
        .host(broken.id())
        .expect("the host whose worker panicked");
    assert_eq!(failed.state(), HostState::Failed);
    assert!(
        failed.detail().expect("a reason").contains("stopped"),
        "the reason says the worker stopped: {:?}",
        failed.detail()
    );
    assert_eq!(
        run.host(other.id()).expect("the other host").state(),
        HostState::Succeeded
    );
}

#[test]
fn a_batch_runs_two_steps_over_a_real_server_when_one_can_be_started() {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED a_batch_runs_two_steps_over_a_real_server: this host cannot start a disposable OpenSSH server"
        );
        return;
    }
    let server = Harness::start("batch", Harness::free_port()).expect("a server");
    let mut remote =
        HostConfig::new("batch-target", "127.0.0.1", server.port(), server.user()).expect("a host");
    let _ = &mut remote;
    let snapshot = target(&remote);
    let mut task = BatchTask::new(
        vec![
            BatchStep::new("printf 'one\\n'").expect("a step"),
            BatchStep::new("printf 'two\\n'; exit 3").expect("a step"),
        ],
        vec![snapshot.clone()],
        1,
    )
    .expect("a task");
    task.set_failure_policy(FailurePolicy::SkipHost);
    let routes = Arc::new(SshRoutes::new());
    routes.insert(HostRoute::Direct {
        host: Box::new(remote.clone()),
        auth: AuthMethod::password(server.password()),
        known: KnownHostKey::Trusted {
            fingerprint: server.host_key_fingerprint(),
        },
    });
    let confirmed = task.confirm(1_700_000_010).expect("a confirmable task");
    let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 1_700_000_011);
    let mut runner = BatchRunner::start(
        confirmed,
        Arc::new(SshTransports::new(routes)),
        RunnerOptions { concurrency: 1 },
    );
    let mut events = Vec::new();
    assert!(wait_for(&mut runner, &mut events, |events| {
        events.iter().any(|event| {
            matches!(
                event.kind(),
                BatchEventKind::RunState { state } if state.is_finished()
            )
        })
    }));
    events.extend(runner.join());
    fold(&mut run, &events);

    let record = run.host(remote.id()).expect("the host");
    assert_eq!(record.steps()[0].state(), StepState::Succeeded);
    assert_eq!(record.steps()[0].exit(), Some(0));
    assert_eq!(record.steps()[1].state(), StepState::Failed);
    assert_eq!(record.steps()[1].exit(), Some(3));
    let output: String = events
        .iter()
        .filter_map(|event| match event.kind() {
            BatchEventKind::Output { chunk, .. } => Some(chunk.clone()),
            _ => None,
        })
        .collect();
    assert!(
        output.contains("one"),
        "the first command's output was recorded: {output}"
    );
    assert!(
        output.contains("two"),
        "the second command's output was recorded: {output}"
    );
}
