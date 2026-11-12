//! Ordered multi-step batch tasks over a frozen set of hosts (R19, R21).
//!
//! A batch task is a list of steps that runs on every target, in order, and nothing
//! else. There is deliberately no conditional step, no variable evaluation, no
//! branch that depends on another host's result, no approval node and no rollback:
//! R21 keeps this feature a task runner, not a workflow engine. What the model does
//! carry is the control R19 asks for - the target snapshot, the concurrency limit,
//! the failure policy, and a run history that can say what actually happened on
//! which host at which step.
//!
//! Four rules shape the types:
//!
//! * a task only becomes runnable by being confirmed. `BatchTask` is an editable
//!   definition; `ConfirmedBatch` is that definition frozen at confirmation time.
//!   Nothing in this module opens a connection, so building, saving or previewing a
//!   task can never start a session; only a confirmed batch can be handed to a
//!   runner.
//! * a target is a snapshot, not a reference. The address, user, jump list and
//!   resolved connection policy are copied when the task is confirmed, so editing a
//!   group or a host afterwards cannot change what a confirmed batch runs against.
//! * every host has its own run, every step has its own state and exit result. One
//!   host failing skips only that host's remaining steps; the others keep going.
//! * a retry is a new task that contains only the work still owed: the hosts that
//!   did not succeed and, per host, only the steps that did not already succeed.
//!   It is not a rollback: whatever ran on the remote machine stays there.
//!
//! The run is written down as a fold over events (`BatchRun::apply`), which is also
//! how the append-only log is read back: the summary and the log are the same
//! sequence of statements, so they cannot disagree.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthMethodKind;
use crate::connection::{ConnectionDefaults, ConnectionPolicy};
use crate::host::HostConfig;
use crate::ids::HostId;

/// The format this build writes for the saved batch tasks and runs.
pub const BATCH_FORMAT_VERSION: u32 = 1;

/// The most steps one task may hold.
///
/// A batch task is a short sequence a person reads before confirming it; the limit
/// keeps a task from becoming a script nobody reviewed.
pub const MAX_BATCH_STEPS: usize = 32;

/// The most hosts one task may target.
///
/// This is the scale R18 promises for the host list, so a "run on everything I can
/// see" selection is still expressible; the concurrency limit is what keeps it from
/// becoming thousands of sessions at once.
pub const MAX_BATCH_TARGETS: usize = 5_000;

/// The most sessions one task may have open at the same time.
pub const MAX_BATCH_CONCURRENCY: u32 = 32;

/// The concurrency a new task starts with.
pub const DEFAULT_BATCH_CONCURRENCY: u32 = 4;

/// The longest command one step may hold, in characters.
pub const MAX_STEP_COMMAND_CHARS: usize = 8_000;

/// The longest target label a snapshot keeps, in characters.
pub const MAX_TARGET_LABEL_CHARS: usize = 128;

/// The longest failure reason kept for one host or step, in characters.
pub const MAX_BATCH_DETAIL_CHARS: usize = 400;

/// Identifies one saved batch task.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchTaskId(Uuid);

impl BatchTaskId {
    /// Creates a fresh random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an identifier that was persisted earlier.
    #[must_use]
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Returns the underlying value, for storage keys and logging.
    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for BatchTaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BatchTaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Debug for BatchTaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "BatchTaskId({})", self.0)
    }
}

/// Identifies one execution of a batch task.
///
/// A retry is a new run with its own identifier, linked from the task it was built
/// from, so the history shows two runs rather than one run rewritten.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchRunId(Uuid);

impl BatchRunId {
    /// Creates a fresh random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an identifier that was persisted earlier.
    #[must_use]
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Returns the underlying value, for storage keys and logging.
    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    /// Reads an identifier from its text form, which is how a store names the file a
    /// run lives in and how a reader recovers it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Uuid::parse_str(text).ok().map(Self)
    }
}

impl Default for BatchRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BatchRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Debug for BatchRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "BatchRunId({})", self.0)
    }
}

/// Which stream a log line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    /// The command's standard output.
    Stdout,
    /// The command's standard error.
    Stderr,
}

/// What a host's remaining steps do after one of them fails (R19).
///
/// Neither choice is a rollback: a step that already ran has already had its effect
/// on the remote machine, and nothing here claims otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// The failed host stops and its remaining steps are skipped; other hosts keep
    /// going. This is the default because one unreachable server must not cancel a
    /// task the user aimed at a whole group.
    #[default]
    SkipHost,
    /// The first failure stops scheduling: running steps finish, no new step starts,
    /// and the hosts that had not started are skipped.
    StopAfterFailure,
}

impl FailurePolicy {
    /// Every policy, in the order the preview offers them.
    pub const ALL: [Self; 2] = [Self::SkipHost, Self::StopAfterFailure];

    /// The sentence a preview shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SkipHost => "skip the rest of that host, keep the other hosts going",
            Self::StopAfterFailure => "stop starting new steps after the first failure",
        }
    }
}

/// Where one host is in its own run (R19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    /// Waiting for a slot under the concurrency limit.
    Queued,
    /// Dialling, before the user is asked anything.
    Connecting,
    /// Waiting for the user to accept an unknown identity, or to confirm a
    /// ProxyCommand. Only this host waits; the rest of the task does not.
    WaitingForUser,
    /// Connected and running a step.
    Running,
    /// Every step this host owed finished with exit status 0.
    Succeeded,
    /// A step failed, the connection failed, or the user refused the question.
    Failed,
    /// Never started because the task stopped scheduling first.
    Skipped,
    /// Stopped because the user cancelled the run, after the remote command had
    /// really ended.
    Cancelled,
    /// The application ended while this host still had work to do. A restart marks
    /// it so; nothing is replayed automatically.
    Interrupted,
}

impl HostState {
    /// Whether the host has reached its final state for this run.
    #[must_use]
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Cancelled | Self::Interrupted
        )
    }

    /// The sentence a result list shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Connecting => "connecting",
            Self::WaitingForUser => "waiting for you",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Where one step of one host is (R19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Not started yet.
    Pending,
    /// The command was sent and has not exited yet.
    Running,
    /// The remote command exited with status 0.
    Succeeded,
    /// The remote command exited with a non-zero status, or never started.
    Failed,
    /// Skipped because this host failed earlier, or because the task stopped.
    Skipped,
    /// The channel was closed by a cancel request and the remote command really
    /// ended.
    Cancelled,
    /// The application ended while this step was still running.
    Interrupted,
    /// Finished by an earlier run of the same task; a retry does not run it again.
    AlreadyDone,
}

impl StepState {
    /// Whether this step is finished for the run it belongs to.
    #[must_use]
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }

    /// Whether this step counts as a success a retry may skip.
    #[must_use]
    pub fn is_successful(self) -> bool {
        matches!(self, Self::Succeeded | Self::AlreadyDone)
    }

    /// The sentence a result list shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::AlreadyDone => "already done",
        }
    }
}

/// Where a whole run is (R19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Hosts are being run.
    Running,
    /// No new step starts; a step already running finishes.
    Paused,
    /// The user asked to stop. This state lasts until every running command has
    /// really ended, which is why it is not called "cancelled".
    Cancelling,
    /// Every step of every host finished with status 0.
    Completed,
    /// At least one host failed and the user did not cancel.
    Failed,
    /// The user cancelled and every running command has ended.
    Cancelled,
    /// The application ended before the run did; a restart marks it and never
    /// replays it.
    Interrupted,
}

impl RunState {
    /// Whether the run has reached a final state.
    #[must_use]
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    /// Whether this state may move to `next`.
    ///
    /// The point of the list is that a run cannot be shown as finished while a
    /// command is still running, and cannot be pushed back to "running" once it was
    /// cancelled or interrupted.
    #[must_use]
    pub fn can_move_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Running => matches!(next, Self::Paused | Self::Cancelling) || next.is_finished(),
            Self::Paused => matches!(next, Self::Running | Self::Cancelling | Self::Interrupted),
            Self::Cancelling => matches!(next, Self::Cancelled | Self::Failed | Self::Interrupted),
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted => false,
        }
    }

    /// The sentence the run header shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Why a batch task, target, run or transition was refused.
///
/// Each variant is a sentence a preview or a result list can show, because
/// "invalid task" does not tell a user which step or which host to fix.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BatchProblem {
    /// A task needs at least one step.
    #[error("a batch task needs at least one step; nothing would run")]
    NoSteps,
    /// A task needs at least one target.
    #[error("a batch task needs at least one host; select hosts before confirming")]
    NoTargets,
    /// More steps than `MAX_BATCH_STEPS`.
    #[error("a batch task holds at most {max} steps; this one has {found}")]
    TooManySteps {
        /// The limit that applies.
        max: usize,
        /// How many steps were given.
        found: usize,
    },
    /// More targets than `MAX_BATCH_TARGETS`.
    #[error("a batch task holds at most {max} hosts; this one has {found}")]
    TooManyTargets {
        /// The limit that applies.
        max: usize,
        /// How many hosts were given.
        found: usize,
    },
    /// A step's command is empty or only spaces.
    #[error("step {step} has no command; a step that runs nothing cannot be confirmed")]
    EmptyCommand {
        /// One-based number of the step.
        step: usize,
    },
    /// A step's command is longer than `MAX_STEP_COMMAND_CHARS`.
    #[error("step {step} is longer than {max} characters; shorten it and confirm again")]
    CommandTooLong {
        /// One-based number of the step.
        step: usize,
        /// The limit that applies.
        max: usize,
    },
    /// The same host is in the target list twice.
    #[error("the host {label} is in this task twice; each host runs the steps once")]
    DuplicateTarget {
        /// The label of the repeated host.
        label: String,
    },
    /// The concurrency limit is outside `1..=MAX_BATCH_CONCURRENCY`.
    #[error("a task runs 1 to {max} hosts at once; {found} was asked for")]
    ConcurrencyOutOfRange {
        /// The limit that applies.
        max: u32,
        /// The value that was asked for.
        found: u32,
    },
    /// A target label is empty or too long.
    #[error("a host label must be 1 to {max} characters")]
    BadTargetLabel {
        /// The limit that applies.
        max: usize,
    },
    /// A target address or user is empty.
    #[error("the host {label} has no address or no user; fix it before confirming")]
    BadTargetAddress {
        /// The label of the host.
        label: String,
    },
    /// A target's start step is past the end of the step list.
    #[error("the host {label} starts at step {step}, but the task has only {steps} steps")]
    StartStepOutOfRange {
        /// The label of the host.
        label: String,
        /// The step index that was asked for.
        step: u32,
        /// How many steps the task has.
        steps: usize,
    },
    /// A snapshot's connection values could not be resolved.
    #[error("the connection policy of {label} is not usable: {reason}")]
    BadPolicy {
        /// The label of the host.
        label: String,
        /// What the connection layer reported.
        reason: String,
    },
    /// A state change the run model does not allow.
    #[error("the run cannot move from {from} to {to}")]
    InvalidTransition {
        /// The state the run or host is in.
        from: &'static str,
        /// The state that was asked for.
        to: &'static str,
    },
    /// An event named a host or step this run does not have.
    #[error("the event names a host or step this run does not have")]
    UnknownTarget,
    /// There is nothing left to retry.
    #[error("nothing to retry: every host and step already succeeded")]
    NothingToRetry,
}

fn check_command(step: usize, command: &str) -> Result<(), BatchProblem> {
    if command.trim().is_empty() {
        return Err(BatchProblem::EmptyCommand { step });
    }
    if command.chars().count() > MAX_STEP_COMMAND_CHARS {
        return Err(BatchProblem::CommandTooLong {
            step,
            max: MAX_STEP_COMMAND_CHARS,
        });
    }
    Ok(())
}

fn short_detail(reason: &str) -> String {
    let mut text: String = reason.chars().take(MAX_BATCH_DETAIL_CHARS).collect();
    if text.chars().count() < reason.chars().count() {
        text.push('…');
    }
    text
}

/// One step: a command line, run in order after the step before it (R21).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchStep {
    command: String,
}

impl BatchStep {
    /// A step from the text the user typed.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::EmptyCommand` or `BatchProblem::CommandTooLong`; the
    /// one-based step number is added by `BatchTask::new`, which knows it.
    pub fn new(command: impl Into<String>) -> Result<Self, BatchProblem> {
        let command = command.into();
        check_command(0, &command)?;
        Ok(Self { command })
    }

    /// The command line this step runs.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
}

/// One host, as it was when the task was confirmed (R19).
///
/// The snapshot copies what a run needs to dial and what a result list needs to name
/// the host. It copies no credential: a password, a passphrase and an agent choice
/// are resolved for each attempt by the layer that dials, so a stored task file never
/// becomes a place a secret lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchTarget {
    host_id: HostId,
    label: String,
    address: String,
    port: u16,
    user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<AuthMethodKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    jumps: Vec<HostId>,
    connection: ConnectionDefaults,
    #[serde(default, skip_serializing_if = "is_zero")]
    starts_at: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl BatchTarget {
    /// The snapshot of one saved host.
    ///
    /// The connection policy is resolved here, against the global defaults, so a
    /// later change to those defaults cannot alter a task that was already
    /// confirmed.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::BadPolicy` when the resolved numbers are not a policy
    /// this build accepts.
    pub fn snapshot(host: &HostConfig, defaults: ConnectionDefaults) -> Result<Self, BatchProblem> {
        let own = host.connection();
        let label = host.name().to_owned();
        let connection = ConnectionDefaults::new(
            own.and_then(|value| value.keepalive_interval_secs())
                .unwrap_or(defaults.keepalive_interval_secs()),
            own.and_then(|value| value.keepalive_max_misses())
                .unwrap_or(defaults.keepalive_max_misses()),
            own.and_then(|value| value.connect_timeout_secs())
                .unwrap_or(defaults.connect_timeout_secs()),
        )
        .map_err(|error| BatchProblem::BadPolicy {
            label: label.clone(),
            reason: error.to_string(),
        })?;
        Ok(Self {
            host_id: host.id(),
            label: host.name().to_owned(),
            address: host.address().to_owned(),
            port: host.port().get(),
            user: host.user().to_owned(),
            auth: host.auth(),
            jumps: host.jumps().to_vec(),
            connection,
            starts_at: 0,
        })
    }

    /// A target built from values, for a reader that already validated them.
    ///
    /// # Errors
    ///
    /// Returns the label or address problem `BatchTarget::validate` would report.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host_id: HostId,
        label: impl Into<String>,
        address: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        auth: Option<AuthMethodKind>,
        jumps: Vec<HostId>,
        connection: ConnectionDefaults,
    ) -> Result<Self, BatchProblem> {
        let target = Self {
            host_id,
            label: label.into(),
            address: address.into(),
            port,
            user: user.into(),
            auth,
            jumps,
            connection,
            starts_at: 0,
        };
        target.validate()?;
        Ok(target)
    }

    /// Checks the fields a task file or a caller could have got wrong.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::BadTargetLabel` or `BatchProblem::BadTargetAddress`.
    pub fn validate(&self) -> Result<(), BatchProblem> {
        let label_len = self.label.chars().count();
        if label_len == 0 || label_len > MAX_TARGET_LABEL_CHARS {
            return Err(BatchProblem::BadTargetLabel {
                max: MAX_TARGET_LABEL_CHARS,
            });
        }
        if self.address.trim().is_empty() || self.user.trim().is_empty() {
            return Err(BatchProblem::BadTargetAddress {
                label: self.label.clone(),
            });
        }
        Ok(())
    }

    /// The saved host this target was copied from.
    #[must_use]
    pub fn host_id(&self) -> HostId {
        self.host_id
    }

    /// The host name at confirmation time.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The address the run dials.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The port the run dials.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The login user the run dials with.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The sign-in kind the host had, for display only.
    #[must_use]
    pub fn auth(&self) -> Option<AuthMethodKind> {
        self.auth
    }

    /// The jump hosts the saved host named, for display and for the chain dial.
    #[must_use]
    pub fn jumps(&self) -> &[HostId] {
        &self.jumps
    }

    /// The resolved connection policy, frozen at confirmation.
    #[must_use]
    pub fn connection(&self) -> ConnectionDefaults {
        self.connection
    }

    /// The keepalive and deadline this target's attempt uses (R20).
    #[must_use]
    pub fn policy(&self) -> ConnectionPolicy {
        self.connection.policy()
    }

    /// The first step this target runs.
    ///
    /// Non-zero only on a retry, where the steps before it already succeeded.
    #[must_use]
    pub fn starts_at(&self) -> u32 {
        self.starts_at
    }

    /// The same target starting at another step.
    #[must_use]
    pub fn with_start_step(mut self, step: u32) -> Self {
        self.starts_at = step;
        self
    }

    /// `user@address:port`, the form the result list shows.
    #[must_use]
    pub fn destination(&self) -> String {
        format!("{}@{}:{}", self.user, self.address, self.port)
    }
}

/// A batch task as it is edited and saved: ordered steps, a frozen target list and
/// the two controls a preview shows (R19).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchTask {
    id: BatchTaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    steps: Vec<BatchStep>,
    targets: Vec<BatchTarget>,
    concurrency: u32,
    on_failure: FailurePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_of: Option<BatchRunId>,
}

impl BatchTask {
    /// A task from steps and the hosts it was built for.
    ///
    /// # Errors
    ///
    /// Returns the step problem, the target problem, the duplicate host, the
    /// concurrency range problem or a limit: everything a confirmation has to be
    /// able to promise is checked here, not when the run starts.
    pub fn new(
        steps: Vec<BatchStep>,
        targets: Vec<BatchTarget>,
        concurrency: u32,
    ) -> Result<Self, BatchProblem> {
        let task = Self {
            id: BatchTaskId::new(),
            name: None,
            steps,
            targets,
            concurrency,
            on_failure: FailurePolicy::default(),
            retry_of: None,
        };
        task.validate()?;
        Ok(task)
    }

    /// Steps from command text, in the order they will run.
    ///
    /// # Errors
    ///
    /// Returns the offending step's `BatchProblem`.
    pub fn steps_from_commands(
        commands: impl IntoIterator<Item = String>,
    ) -> Result<Vec<BatchStep>, BatchProblem> {
        commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                check_command(index + 1, &command)?;
                Ok(BatchStep { command })
            })
            .collect()
    }

    /// Checks every part of the definition.
    ///
    /// # Errors
    ///
    /// Returns the first problem found, in the order a preview reads the task.
    pub fn validate(&self) -> Result<(), BatchProblem> {
        if self.steps.is_empty() {
            return Err(BatchProblem::NoSteps);
        }
        if self.steps.len() > MAX_BATCH_STEPS {
            return Err(BatchProblem::TooManySteps {
                max: MAX_BATCH_STEPS,
                found: self.steps.len(),
            });
        }
        if self.targets.is_empty() {
            return Err(BatchProblem::NoTargets);
        }
        if self.targets.len() > MAX_BATCH_TARGETS {
            return Err(BatchProblem::TooManyTargets {
                max: MAX_BATCH_TARGETS,
                found: self.targets.len(),
            });
        }
        if self.concurrency == 0 || self.concurrency > MAX_BATCH_CONCURRENCY {
            return Err(BatchProblem::ConcurrencyOutOfRange {
                max: MAX_BATCH_CONCURRENCY,
                found: self.concurrency,
            });
        }
        for (index, step) in self.steps.iter().enumerate() {
            check_command(index + 1, step.command())?;
        }
        let mut seen = std::collections::BTreeSet::new();
        for target in &self.targets {
            target.validate()?;
            if !seen.insert(target.host_id) {
                return Err(BatchProblem::DuplicateTarget {
                    label: target.label().to_owned(),
                });
            }
            let start = target.starts_at();
            if start as usize >= self.steps.len() {
                return Err(BatchProblem::StartStepOutOfRange {
                    label: target.label().to_owned(),
                    step: start,
                    steps: self.steps.len(),
                });
            }
        }
        Ok(())
    }

    /// The identifier this task keeps.
    #[must_use]
    pub fn id(&self) -> BatchTaskId {
        self.id
    }

    /// The name a list shows, when the user gave one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The ordered steps.
    #[must_use]
    pub fn steps(&self) -> &[BatchStep] {
        &self.steps
    }

    /// The frozen targets.
    #[must_use]
    pub fn targets(&self) -> &[BatchTarget] {
        &self.targets
    }

    /// How many hosts may be connected at once.
    #[must_use]
    pub fn concurrency(&self) -> u32 {
        self.concurrency
    }

    /// What a failure does to the rest of the task.
    #[must_use]
    pub fn on_failure(&self) -> FailurePolicy {
        self.on_failure
    }

    /// The earlier run this task retries, when it is a retry.
    #[must_use]
    pub fn retry_of(&self) -> Option<BatchRunId> {
        self.retry_of
    }

    /// Whether this definition is a retry of an earlier run.
    #[must_use]
    pub fn is_retry(&self) -> bool {
        self.retry_of.is_some()
    }

    /// Sets or clears the name shown in the list.
    pub fn set_name(&mut self, name: Option<impl Into<String>>) {
        self.name = name.map(Into::into);
    }

    /// Sets what a failure does to the rest of the task.
    pub fn set_failure_policy(&mut self, policy: FailurePolicy) {
        self.on_failure = policy;
    }

    /// Sets the concurrency limit.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::ConcurrencyOutOfRange` for `0` and for values above
    /// `MAX_BATCH_CONCURRENCY`; the stored value is left as it was.
    pub fn set_concurrency(&mut self, concurrency: u32) -> Result<(), BatchProblem> {
        if concurrency == 0 || concurrency > MAX_BATCH_CONCURRENCY {
            return Err(BatchProblem::ConcurrencyOutOfRange {
                max: MAX_BATCH_CONCURRENCY,
                found: concurrency,
            });
        }
        self.concurrency = concurrency;
        Ok(())
    }

    /// Freezes this definition for running.
    ///
    /// This is the only way to get a `ConfirmedBatch`, and only a confirmed batch
    /// can start a run. Nothing here dials, exercises a credential or opens a
    /// session: a preview that never confirms provably creates no connection.
    ///
    /// # Errors
    ///
    /// Returns the definition problem `BatchTask::validate` reports.
    pub fn confirm(self, at_unix: u64) -> Result<ConfirmedBatch, BatchProblem> {
        self.validate()?;
        Ok(ConfirmedBatch {
            task: self,
            confirmed_at_unix: at_unix,
        })
    }

    /// The retry of a finished run: only the hosts that did not succeed, and per
    /// host only the steps that did not already succeed (R19, R21).
    ///
    /// The new task keeps the same steps, concurrency and failure policy, records
    /// the run it retries, and moves each target's start past the steps that already
    /// succeeded. It is not a rollback: nothing undoes what the earlier run did.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::NothingToRetry` when every host succeeded.
    pub fn retry_after(run: &BatchRun) -> Result<Self, BatchProblem> {
        let mut targets = Vec::new();
        for host in run.hosts() {
            if host.state() == HostState::Succeeded {
                continue;
            }
            let Some(first) = host.first_unfinished_step() else {
                continue;
            };
            targets.push(host.target().clone().with_start_step(first));
        }
        if targets.is_empty() {
            return Err(BatchProblem::NothingToRetry);
        }
        let task = Self {
            id: BatchTaskId::new(),
            name: run.task().name().map(str::to_owned),
            steps: run.task().steps().to_vec(),
            targets,
            concurrency: run.task().concurrency(),
            on_failure: run.task().on_failure(),
            retry_of: Some(run.id()),
        };
        task.validate()?;
        Ok(task)
    }
}

/// A definition frozen at confirmation: the exact targets, steps, concurrency and
/// failure policy a run will use (R19).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedBatch {
    task: BatchTask,
    confirmed_at_unix: u64,
}

impl ConfirmedBatch {
    /// The frozen definition.
    #[must_use]
    pub fn task(&self) -> &BatchTask {
        &self.task
    }

    /// When the user confirmed it.
    #[must_use]
    pub fn confirmed_at_unix(&self) -> u64 {
        self.confirmed_at_unix
    }

    /// How many hosts this batch will run on, which is the number the confirmation
    /// dialog shows.
    #[must_use]
    pub fn target_count(&self) -> usize {
        self.task.targets().len()
    }
}

/// What happened at one step of one host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepRun {
    index: u32,
    state: StepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl StepRun {
    /// The zero-based step this record belongs to.
    #[must_use]
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Where the step is.
    #[must_use]
    pub fn state(&self) -> StepState {
        self.state
    }

    /// The exit status the server reported, when it reported one.
    #[must_use]
    pub fn exit(&self) -> Option<u32> {
        self.exit
    }

    /// Why the step did not succeed, when it did not.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// One host in one run: its snapshot and its own step states.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRun {
    target: BatchTarget,
    state: HostState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    steps: Vec<StepRun>,
}

impl HostRun {
    /// The frozen target this run used.
    #[must_use]
    pub fn target(&self) -> &BatchTarget {
        &self.target
    }

    /// Where this host is.
    #[must_use]
    pub fn state(&self) -> HostState {
        self.state
    }

    /// Why the host did not succeed, when it did not.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Every step record, in order.
    #[must_use]
    pub fn steps(&self) -> &[StepRun] {
        &self.steps
    }

    /// The first step that has not succeeded, which is what a retry runs from.
    #[must_use]
    pub fn first_unfinished_step(&self) -> Option<u32> {
        self.steps
            .iter()
            .find(|step| !step.state.is_successful())
            .map(|step| step.index)
    }

    fn step_mut(&mut self, index: u32) -> Option<&mut StepRun> {
        self.steps.iter_mut().find(|step| step.index == index)
    }
}

/// One execution of one confirmed batch, with the per-host and per-step record the
/// result list reads (R19).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchRun {
    id: BatchRunId,
    state: RunState,
    task: BatchTask,
    hosts: Vec<HostRun>,
    started_at_unix: u64,
}

impl BatchRun {
    /// A run planned from a confirmed batch: every host queued, every step pending,
    /// and the steps a retry does not repeat already marked.
    #[must_use]
    pub fn planned(confirmed: &ConfirmedBatch, id: BatchRunId, at_unix: u64) -> Self {
        let task = confirmed.task().clone();
        let hosts = task
            .targets()
            .iter()
            .map(|target| {
                let steps = task
                    .steps()
                    .iter()
                    .enumerate()
                    .map(|(index, _)| StepRun {
                        index: index as u32,
                        state: if (index as u32) < target.starts_at() {
                            StepState::AlreadyDone
                        } else {
                            StepState::Pending
                        },
                        exit: None,
                        detail: None,
                    })
                    .collect();
                HostRun {
                    target: target.clone(),
                    state: HostState::Queued,
                    detail: None,
                    steps,
                }
            })
            .collect();
        Self {
            id,
            state: RunState::Running,
            task,
            hosts,
            started_at_unix: at_unix,
        }
    }

    /// The identifier of this run.
    #[must_use]
    pub fn id(&self) -> BatchRunId {
        self.id
    }

    /// Where the run is.
    #[must_use]
    pub fn state(&self) -> RunState {
        self.state
    }

    /// The frozen task this run executes.
    #[must_use]
    pub fn task(&self) -> &BatchTask {
        &self.task
    }

    /// Every host of this run, in confirmation order.
    #[must_use]
    pub fn hosts(&self) -> &[HostRun] {
        &self.hosts
    }

    /// When the run started.
    #[must_use]
    pub fn started_at_unix(&self) -> u64 {
        self.started_at_unix
    }

    /// The host record for one target.
    #[must_use]
    pub fn host(&self, host: HostId) -> Option<&HostRun> {
        self.hosts
            .iter()
            .find(|record| record.target.host_id() == host)
    }

    /// How many hosts reached a final state.
    #[must_use]
    pub fn finished_hosts(&self) -> usize {
        self.hosts
            .iter()
            .filter(|record| record.state.is_finished())
            .count()
    }

    /// Whether any host failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.hosts
            .iter()
            .any(|record| record.state() == HostState::Failed)
    }

    /// The run state a finished task deserves, given what the hosts did.
    ///
    /// It is derived rather than asserted by the caller: a run is only "completed"
    /// when every host succeeded, and a cancelled or interrupted run stays that way
    /// even if some hosts had already failed.
    #[must_use]
    pub fn outcome(&self) -> RunState {
        if self
            .hosts
            .iter()
            .any(|record| record.state() == HostState::Interrupted)
        {
            return RunState::Interrupted;
        }
        if self
            .hosts
            .iter()
            .any(|record| record.state() == HostState::Cancelled)
        {
            return RunState::Cancelled;
        }
        if self.has_failures()
            || self
                .hosts
                .iter()
                .any(|record| record.state() == HostState::Skipped)
        {
            return RunState::Failed;
        }
        RunState::Completed
    }

    /// Folds one logged event into this run.
    ///
    /// Both the saved summary and the append-only log go through this function, so
    /// what the log says and what the summary says cannot drift apart. An event for
    /// a host or step this run does not have is refused instead of being guessed at.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::UnknownTarget` for an unknown host or step, or
    /// `BatchProblem::InvalidTransition` for a state change the model refuses.
    pub fn apply(&mut self, event: &BatchEvent) -> Result<(), BatchProblem> {
        self.apply_kind(event.kind())
    }

    /// Folds one event kind into this run, for a caller that has not built a
    /// positioned event yet.
    ///
    /// # Errors
    ///
    /// The same problems [`Self::apply`] reports.
    pub fn apply_kind(&mut self, event: &BatchEventKind) -> Result<(), BatchProblem> {
        match event {
            BatchEventKind::RunStarted { .. } => {
                self.move_to(RunState::Running)?;
            }
            BatchEventKind::RunState { state } => {
                self.move_to(*state)?;
            }
            BatchEventKind::HostState { host, state } => {
                let record = self
                    .hosts
                    .iter_mut()
                    .find(|record| record.target.host_id() == *host)
                    .ok_or(BatchProblem::UnknownTarget)?;
                record.state = *state;
            }
            BatchEventKind::TrustPending { host, .. } => {
                let record = self
                    .hosts
                    .iter_mut()
                    .find(|record| record.target.host_id() == *host)
                    .ok_or(BatchProblem::UnknownTarget)?;
                record.state = HostState::WaitingForUser;
            }
            BatchEventKind::TrustDecided { host, accepted } => {
                let record = self
                    .hosts
                    .iter_mut()
                    .find(|record| record.target.host_id() == *host)
                    .ok_or(BatchProblem::UnknownTarget)?;
                record.state = if *accepted {
                    HostState::Connecting
                } else {
                    HostState::Failed
                };
                if !*accepted {
                    record.detail = Some("the host identity was not accepted".to_owned());
                }
            }
            BatchEventKind::StepStarted { host, step } => {
                let record = self
                    .hosts
                    .iter_mut()
                    .find(|record| record.target.host_id() == *host)
                    .ok_or(BatchProblem::UnknownTarget)?;
                record.state = HostState::Running;
                let step = record.step_mut(*step).ok_or(BatchProblem::UnknownTarget)?;
                step.state = StepState::Running;
            }
            BatchEventKind::StepFinished {
                host,
                step,
                state,
                exit,
                detail,
            } => {
                let record = self
                    .hosts
                    .iter_mut()
                    .find(|record| record.target.host_id() == *host)
                    .ok_or(BatchProblem::UnknownTarget)?;
                let step = record.step_mut(*step).ok_or(BatchProblem::UnknownTarget)?;
                step.state = *state;
                step.exit = *exit;
                step.detail = detail.as_deref().map(short_detail);
            }
            BatchEventKind::HostFinished {
                host,
                state,
                detail,
            } => {
                let record = self
                    .hosts
                    .iter_mut()
                    .find(|record| record.target.host_id() == *host)
                    .ok_or(BatchProblem::UnknownTarget)?;
                record.state = *state;
                record.detail = detail.as_deref().map(short_detail);
                let skip = match state {
                    HostState::Succeeded => StepState::Skipped,
                    HostState::Cancelled => StepState::Cancelled,
                    HostState::Interrupted => StepState::Interrupted,
                    HostState::Failed | HostState::Skipped => StepState::Skipped,
                    HostState::Queued
                    | HostState::Connecting
                    | HostState::WaitingForUser
                    | HostState::Running => return Ok(()),
                };
                for step in &mut record.steps {
                    if !step.state.is_finished() {
                        step.state = skip;
                    }
                }
            }
            BatchEventKind::Output { .. } | BatchEventKind::ProxyPending { .. } => {}
        }
        Ok(())
    }

    /// Moves the whole run to another state.
    ///
    /// # Errors
    ///
    /// Returns `BatchProblem::InvalidTransition` when the model refuses the move,
    /// for example anything after the run was cancelled.
    pub fn move_to(&mut self, next: RunState) -> Result<(), BatchProblem> {
        if !self.state.can_move_to(next) {
            return Err(BatchProblem::InvalidTransition {
                from: self.state.label(),
                to: next.label(),
            });
        }
        self.state = next;
        Ok(())
    }

    /// Re-reads the run after it was loaded from disk, refusing a record this build
    /// would not have written.
    ///
    /// # Errors
    ///
    /// Returns the task problem, or a host whose step list does not match the task.
    pub fn validate(&self) -> Result<(), BatchProblem> {
        self.task.validate()?;
        for record in &self.hosts {
            if record.steps.len() != self.task.steps().len() {
                return Err(BatchProblem::UnknownTarget);
            }
        }
        Ok(())
    }

    /// Marks a run as interrupted by a restart, keeping what already finished.
    ///
    /// Steps that were running when the application ended become
    /// `StepState::Interrupted` rather than staying "running" forever, and nothing
    /// is replayed.
    pub fn mark_interrupted(&mut self) {
        if self.state.is_finished() {
            return;
        }
        self.state = RunState::Interrupted;
        for record in &mut self.hosts {
            if !record.state.is_finished() {
                record.state = HostState::Interrupted;
                if record.detail.is_none() {
                    record.detail = Some(
                        "the application ended while this host still had steps to run".to_owned(),
                    );
                }
                for step in &mut record.steps {
                    if !step.state.is_finished() {
                        step.state = StepState::Interrupted;
                    }
                }
            }
        }
    }
}

/// The saved batch tasks, in one versioned document.
///
/// A task is a definition, not a run: opening the file starts nothing and dials
/// nothing. Targets inside a task are already snapshots, so a saved task is exactly
/// what a confirmation would run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchBook {
    #[serde(default = "book_version")]
    version: u32,
    #[serde(default)]
    tasks: Vec<BatchTask>,
}

fn book_version() -> u32 {
    BATCH_FORMAT_VERSION
}

impl BatchBook {
    /// An empty library at the current format.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: BATCH_FORMAT_VERSION,
            tasks: Vec::new(),
        }
    }

    /// The format this document declares.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Every saved task, in the order the list shows them.
    #[must_use]
    pub fn tasks(&self) -> &[BatchTask] {
        &self.tasks
    }

    /// One saved task.
    #[must_use]
    pub fn task(&self, id: BatchTaskId) -> Option<&BatchTask> {
        self.tasks.iter().find(|task| task.id() == id)
    }

    /// Adds a task, replacing a record with the same identifier.
    pub fn insert(&mut self, task: BatchTask) {
        if let Some(existing) = self.tasks.iter_mut().find(|saved| saved.id() == task.id()) {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
    }

    /// Removes a task and reports whether it was there.
    pub fn remove(&mut self, id: BatchTaskId) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|task| task.id() != id);
        before != self.tasks.len()
    }

    /// Checks a document that was read from disk.
    ///
    /// # Errors
    ///
    /// Returns a reason naming the stored task that this build would not have
    /// written: a newer format, a repeated identifier or an invalid definition.
    pub fn validate(&self) -> Result<(), String> {
        if self.version > BATCH_FORMAT_VERSION {
            return Err(format!(
                "the file uses format {}, but this build writes {BATCH_FORMAT_VERSION}",
                self.version
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for task in &self.tasks {
            if !seen.insert(task.id()) {
                return Err(format!("the task {} is stored twice", task.id()));
            }
            task.validate()
                .map_err(|problem| format!("the task {} is not valid: {problem}", task.id()))?;
        }
        Ok(())
    }
}

/// One statement in a run's append-only log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchEvent {
    seq: u64,
    at_unix: u64,
    kind: BatchEventKind,
}

impl BatchEvent {
    /// An event with its position in the log and the moment it happened.
    #[must_use]
    pub fn new(seq: u64, at_unix: u64, kind: BatchEventKind) -> Self {
        Self { seq, at_unix, kind }
    }

    /// The position in the log, starting at zero and increasing by one.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// When it happened.
    #[must_use]
    pub fn at_unix(&self) -> u64 {
        self.at_unix
    }

    /// What happened.
    #[must_use]
    pub fn kind(&self) -> &BatchEventKind {
        &self.kind
    }

    /// The host this event belongs to, so a per-host log view can filter.
    #[must_use]
    pub fn host(&self) -> Option<HostId> {
        match &self.kind {
            BatchEventKind::HostState { host, .. }
            | BatchEventKind::TrustPending { host, .. }
            | BatchEventKind::ProxyPending { host, .. }
            | BatchEventKind::TrustDecided { host, .. }
            | BatchEventKind::StepStarted { host, .. }
            | BatchEventKind::StepFinished { host, .. }
            | BatchEventKind::HostFinished { host, .. }
            | BatchEventKind::Output { host, .. } => Some(*host),
            BatchEventKind::RunStarted { .. } | BatchEventKind::RunState { .. } => None,
        }
    }
}

/// What one log line says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum BatchEventKind {
    /// The run began, with the numbers the confirmation dialog promised.
    RunStarted {
        /// How many hosts the run has.
        hosts: usize,
        /// How many steps each host runs.
        steps: usize,
        /// The concurrency limit the run will obey.
        concurrency: u32,
    },
    /// The run moved to another state.
    RunState {
        /// The state it moved to.
        state: RunState,
    },
    /// One host moved to another state.
    HostState {
        /// Which host.
        host: HostId,
        /// The state it moved to.
        state: HostState,
    },
    /// An unknown host identity is waiting for the user. Only this host waits.
    TrustPending {
        /// Which host.
        host: HostId,
        /// The host name, for a reader of the log.
        label: String,
        /// The presented key algorithm.
        algorithm: String,
        /// The presented SHA-256 fingerprint.
        fingerprint: String,
    },
    /// A ProxyCommand is waiting for the user's confirmation.
    ProxyPending {
        /// Which host named the command.
        host: HostId,
        /// The command that would run locally.
        command: String,
    },
    /// The user answered a trust or ProxyCommand question.
    TrustDecided {
        /// Which host.
        host: HostId,
        /// Whether the question was accepted.
        accepted: bool,
    },
    /// One step of one host started.
    StepStarted {
        /// Which host.
        host: HostId,
        /// Zero-based step index.
        step: u32,
    },
    /// Bytes a running step produced.
    Output {
        /// Which host.
        host: HostId,
        /// Zero-based step index.
        step: u32,
        /// Which stream they came from.
        stream: OutputStream,
        /// The text, with bytes that are not UTF-8 replaced rather than dropped.
        chunk: String,
    },
    /// One step of one host finished.
    StepFinished {
        /// Which host.
        host: HostId,
        /// Zero-based step index.
        step: u32,
        /// How it ended.
        state: StepState,
        /// The exit status the server reported, when it reported one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<u32>,
        /// Why it did not succeed, when it did not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// One host reached its final state for this run.
    HostFinished {
        /// Which host.
        host: HostId,
        /// The state it ended in.
        state: HostState,
        /// Why, when it did not succeed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::HostConnection;

    fn host(name: &str) -> HostConfig {
        HostConfig::new(name, "192.0.2.10", 22, "deploy").expect("a valid host")
    }

    fn target(host: &HostConfig) -> BatchTarget {
        BatchTarget::snapshot(host, ConnectionDefaults::default()).expect("a snapshot")
    }

    fn task(commands: &[&str], hosts: &[&HostConfig]) -> BatchTask {
        BatchTask::new(
            BatchTask::steps_from_commands(commands.iter().map(|line| (*line).to_owned()))
                .expect("valid steps"),
            hosts.iter().map(|each| target(each)).collect(),
            2,
        )
        .expect("a valid task")
    }

    fn event(seq: u64, kind: BatchEventKind) -> BatchEvent {
        BatchEvent::new(seq, 1_700_000_000, kind)
    }

    #[test]
    fn a_task_without_steps_or_hosts_is_refused() {
        let one = host("alpha");
        assert_eq!(
            BatchTask::new(Vec::new(), vec![target(&one)], 1),
            Err(BatchProblem::NoSteps)
        );
        let steps = BatchTask::steps_from_commands(["uptime".to_owned()]).expect("a step");
        assert_eq!(
            BatchTask::new(steps, Vec::new(), 1),
            Err(BatchProblem::NoTargets)
        );
    }

    #[test]
    fn a_step_that_runs_nothing_is_refused_with_its_number() {
        let error = BatchTask::steps_from_commands(["true".to_owned(), "  ".to_owned()])
            .expect_err("the blank step is refused");
        assert_eq!(error, BatchProblem::EmptyCommand { step: 2 });
        assert!(error.to_string().contains("step 2"));
    }

    #[test]
    fn a_host_cannot_be_in_the_same_task_twice() {
        let one = host("alpha");
        let error = BatchTask::new(
            BatchTask::steps_from_commands(["uptime".to_owned()]).expect("a step"),
            vec![target(&one), target(&one)],
            1,
        )
        .expect_err("the repeat is refused");
        assert_eq!(
            error,
            BatchProblem::DuplicateTarget {
                label: "alpha".to_owned()
            }
        );
    }

    #[test]
    fn the_concurrency_limit_is_bounded_in_both_directions() {
        let one = host("alpha");
        let steps = BatchTask::steps_from_commands(["uptime".to_owned()]).expect("a step");
        for count in [0, MAX_BATCH_CONCURRENCY + 1] {
            let error = BatchTask::new(steps.clone(), vec![target(&one)], count)
                .expect_err("the limit is refused");
            assert_eq!(
                error,
                BatchProblem::ConcurrencyOutOfRange {
                    max: MAX_BATCH_CONCURRENCY,
                    found: count
                }
            );
        }
        let mut task = BatchTask::new(steps, vec![target(&one)], 1).expect("a valid task");
        assert!(task.set_concurrency(3).is_ok());
        assert_eq!(task.concurrency(), 3);
        assert!(task.set_concurrency(0).is_err());
        assert_eq!(task.concurrency(), 3, "a refused change is not applied");
    }

    #[test]
    fn every_task_step_keeps_the_order_it_was_given() {
        let one = host("alpha");
        let task = task(&["one", "two", "three"], &[&one]);
        let commands: Vec<&str> = task.steps().iter().map(BatchStep::command).collect();
        assert_eq!(commands, vec!["one", "two", "three"]);
    }

    #[test]
    fn a_snapshot_freezes_the_connection_policy_instead_of_following_later_changes() {
        let mut one = host("alpha");
        one.set_connection(Some(
            HostConnection::new(Some(45), Some(5), Some(30)).expect("a valid override"),
        ));
        let defaults = ConnectionDefaults::new(30, 3, 20).expect("valid defaults");
        let snapshot = BatchTarget::snapshot(&one, defaults).expect("a snapshot");
        assert_eq!(snapshot.connection().keepalive_interval_secs(), 45);
        assert_eq!(snapshot.connection().keepalive_max_misses(), 5);
        assert_eq!(snapshot.connection().connect_timeout_secs(), 30);
        assert_eq!(
            snapshot.policy().keepalive().map(|keep| keep.interval()),
            Some(std::time::Duration::from_secs(45))
        );
        // The saved host changing afterwards does not reach the snapshot.
        one.set_connection(None);
        assert_eq!(snapshot.connection().keepalive_interval_secs(), 45);
    }

    #[test]
    fn a_host_that_turned_keepalive_off_snapshots_it_as_off() {
        let mut one = host("alpha");
        one.set_connection(Some(HostConnection::keepalive_off()));
        let snapshot =
            BatchTarget::snapshot(&one, ConnectionDefaults::default()).expect("a snapshot");
        assert_eq!(snapshot.connection().keepalive_interval_secs(), 0);
        assert!(snapshot.policy().keepalive().is_none());
    }

    /// Runs a whole successful task through the fold, as the runner would report it.
    fn successful_run(task: &BatchTask) -> BatchRun {
        let confirmed = task.clone().confirm(10).expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        let mut seq = 0;
        let mut emit = |run: &mut BatchRun, kind: BatchEventKind| {
            run.apply(&event(seq, kind)).expect("the fold accepts it");
            seq += 1;
        };
        emit(
            &mut run,
            BatchEventKind::RunStarted {
                hosts: task.targets().len(),
                steps: task.steps().len(),
                concurrency: task.concurrency(),
            },
        );
        for target in task.targets() {
            let host = target.host_id();
            emit(
                &mut run,
                BatchEventKind::HostState {
                    host,
                    state: HostState::Connecting,
                },
            );
            for step in 0..task.steps().len() as u32 {
                emit(&mut run, BatchEventKind::StepStarted { host, step });
                emit(
                    &mut run,
                    BatchEventKind::StepFinished {
                        host,
                        step,
                        state: StepState::Succeeded,
                        exit: Some(0),
                        detail: None,
                    },
                );
            }
            emit(
                &mut run,
                BatchEventKind::HostFinished {
                    host,
                    state: HostState::Succeeded,
                    detail: None,
                },
            );
        }
        let outcome = run.outcome();
        emit(&mut run, BatchEventKind::RunState { state: outcome });
        run
    }

    #[test]
    fn a_run_is_only_completed_when_every_host_succeeded() {
        let one = host("alpha");
        let two = host("beta");
        let task = task(&["uptime"], &[&one, &two]);
        let run = successful_run(&task);
        assert_eq!(run.state(), RunState::Completed);
        assert_eq!(run.outcome(), RunState::Completed);
        assert_eq!(run.finished_hosts(), 2);
        assert!(run.hosts()[0].steps()[0].state().is_successful());
        assert_eq!(run.hosts()[0].steps()[0].exit(), Some(0));
    }

    #[test]
    fn one_failed_host_fails_the_run_and_skips_only_that_host() {
        let one = host("alpha");
        let two = host("beta");
        let task = task(&["one", "two"], &[&one, &two]);
        let confirmed = task.clone().confirm(10).expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        let first = one.id();
        let second = two.id();
        run.apply(&event(
            0,
            BatchEventKind::StepStarted {
                host: first,
                step: 0,
            },
        ))
        .expect("started");
        run.apply(&event(
            1,
            BatchEventKind::StepFinished {
                host: first,
                step: 0,
                state: StepState::Failed,
                exit: Some(2),
                detail: Some("the server refused the command".to_owned()),
            },
        ))
        .expect("failed");
        run.apply(&event(
            2,
            BatchEventKind::HostFinished {
                host: first,
                state: HostState::Failed,
                detail: Some("the server refused the command".to_owned()),
            },
        ))
        .expect("host finished");
        assert_eq!(
            run.host(first).expect("the host").steps()[1].state(),
            StepState::Skipped
        );
        assert_eq!(
            run.host(second).expect("the host").state(),
            HostState::Queued,
            "the other host is untouched"
        );
        assert_eq!(run.outcome(), RunState::Failed);
    }

    #[test]
    fn a_run_cannot_be_pushed_back_to_running_after_it_was_cancelled() {
        let one = host("alpha");
        let task = task(&["uptime"], &[&one]);
        let confirmed = task.confirm(10).expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        run.move_to(RunState::Cancelling).expect("cancelling");
        run.move_to(RunState::Cancelled).expect("cancelled");
        let refused = run.move_to(RunState::Running).expect_err("refused");
        assert_eq!(
            refused,
            BatchProblem::InvalidTransition {
                from: "cancelled",
                to: "running"
            }
        );
    }

    #[test]
    fn an_event_for_an_unknown_host_is_refused() {
        let one = host("alpha");
        let task = task(&["uptime"], &[&one]);
        let confirmed = task.confirm(10).expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        let error = run
            .apply(&event(
                0,
                BatchEventKind::HostState {
                    host: HostId::new(),
                    state: HostState::Running,
                },
            ))
            .expect_err("the host is not in this run");
        assert_eq!(error, BatchProblem::UnknownTarget);
    }

    #[test]
    fn a_retry_keeps_only_the_unfinished_hosts_and_their_unfinished_steps() {
        let one = host("alpha");
        let two = host("beta");
        let task = task(&["one", "two", "three"], &[&one, &two]);
        let confirmed = task.clone().confirm(10).expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        let first = one.id();
        let second = two.id();
        // alpha: step 0 succeeded, step 1 failed, step 2 is skipped.
        run.apply(&event(
            0,
            BatchEventKind::StepStarted {
                host: first,
                step: 0,
            },
        ))
        .expect("started");
        run.apply(&event(
            1,
            BatchEventKind::StepFinished {
                host: first,
                step: 0,
                state: StepState::Succeeded,
                exit: Some(0),
                detail: None,
            },
        ))
        .expect("ok");
        run.apply(&event(
            2,
            BatchEventKind::StepStarted {
                host: first,
                step: 1,
            },
        ))
        .expect("started");
        run.apply(&event(
            3,
            BatchEventKind::StepFinished {
                host: first,
                step: 1,
                state: StepState::Failed,
                exit: Some(1),
                detail: None,
            },
        ))
        .expect("failed");
        run.apply(&event(
            4,
            BatchEventKind::HostFinished {
                host: first,
                state: HostState::Failed,
                detail: None,
            },
        ))
        .expect("finished");
        // beta: every step succeeded.
        let mut seq = 5;
        for step in 0..3u32 {
            run.apply(&event(
                seq,
                BatchEventKind::StepStarted { host: second, step },
            ))
            .expect("started");
            seq += 1;
            run.apply(&event(
                seq,
                BatchEventKind::StepFinished {
                    host: second,
                    step,
                    state: StepState::Succeeded,
                    exit: Some(0),
                    detail: None,
                },
            ))
            .expect("ok");
            seq += 1;
        }
        run.apply(&event(
            seq,
            BatchEventKind::HostFinished {
                host: second,
                state: HostState::Succeeded,
                detail: None,
            },
        ))
        .expect("finished");

        let retry = BatchTask::retry_after(&run).expect("a retry task");
        assert_eq!(
            retry.targets().len(),
            1,
            "the successful host is not retried"
        );
        assert_eq!(retry.targets()[0].host_id(), first);
        assert_eq!(
            retry.targets()[0].starts_at(),
            1,
            "the step that already succeeded is not repeated"
        );
        assert_eq!(retry.steps().len(), 3);
        assert_eq!(retry.retry_of(), Some(run.id()));
        assert!(retry.is_retry());

        let confirmed = retry.clone().confirm(20).expect("the retry is confirmable");
        let retry_run = BatchRun::planned(&confirmed, BatchRunId::new(), 21);
        assert_eq!(
            retry_run.hosts()[0].steps()[0].state(),
            StepState::AlreadyDone
        );
        assert_eq!(retry_run.hosts()[0].steps()[1].state(), StepState::Pending);
    }

    #[test]
    fn a_retry_of_a_fully_successful_run_has_nothing_to_do() {
        let one = host("alpha");
        let task = task(&["uptime"], &[&one]);
        let run = successful_run(&task);
        assert_eq!(
            BatchTask::retry_after(&run).expect_err("nothing to retry"),
            BatchProblem::NothingToRetry
        );
    }

    #[test]
    fn a_restart_marks_an_unfinished_run_interrupted_without_replaying_it() {
        let one = host("alpha");
        let task = task(&["one", "two"], &[&one]);
        let confirmed = task.confirm(10).expect("a confirmable task");
        let mut run = BatchRun::planned(&confirmed, BatchRunId::new(), 11);
        let host_id = one.id();
        run.apply(&event(
            0,
            BatchEventKind::StepStarted {
                host: host_id,
                step: 0,
            },
        ))
        .expect("started");
        run.apply(&event(
            1,
            BatchEventKind::StepFinished {
                host: host_id,
                step: 0,
                state: StepState::Succeeded,
                exit: Some(0),
                detail: None,
            },
        ))
        .expect("ok");
        run.apply(&event(
            2,
            BatchEventKind::StepStarted {
                host: host_id,
                step: 1,
            },
        ))
        .expect("started");
        run.mark_interrupted();
        assert_eq!(run.state(), RunState::Interrupted);
        assert_eq!(run.hosts()[0].state(), HostState::Interrupted);
        assert_eq!(run.hosts()[0].steps()[0].state(), StepState::Succeeded);
        assert_eq!(run.hosts()[0].steps()[1].state(), StepState::Interrupted);
        assert!(run.hosts()[0].detail().is_some());
        // A retry of the interrupted run resumes at the step that was cut off.
        let retry = BatchTask::retry_after(&run).expect("a retry task");
        assert_eq!(retry.targets()[0].starts_at(), 1);
    }

    #[test]
    fn an_event_serializes_with_its_kind_tag_and_refuses_unknown_fields() {
        let encoded = serde_json::to_value(event(
            3,
            BatchEventKind::StepFinished {
                host: HostId::new(),
                step: 1,
                state: StepState::Succeeded,
                exit: Some(0),
                detail: None,
            },
        ))
        .expect("the event serializes");
        assert_eq!(encoded["seq"], 3);
        assert_eq!(encoded["kind"]["event"], "step_finished");
        assert_eq!(encoded["kind"]["state"], "succeeded");
        let decoded: BatchEvent = serde_json::from_value(encoded.clone()).expect("it reads back");
        assert_eq!(decoded.seq(), 3);
        let mut damaged = encoded;
        damaged["kind"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BatchEvent>(damaged).is_err());
    }

    #[test]
    fn a_task_round_trips_through_json_without_credentials() {
        let one = host("alpha");
        let task = task(&["uptime"], &[&one]);
        let encoded = serde_json::to_string(&task).expect("the task serializes");
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("passphrase"));
        let decoded: BatchTask = serde_json::from_str(&encoded).expect("it reads back");
        assert_eq!(decoded, task);
    }

    #[test]
    fn the_saved_book_refuses_a_repeated_task_and_a_newer_format() {
        let one = host("alpha");
        let task = task(&["uptime"], &[&one]);
        let mut book = BatchBook::new();
        book.insert(task.clone());
        assert_eq!(book.tasks().len(), 1);
        assert_eq!(book.task(task.id()), Some(&task));
        book.insert(task.clone());
        assert_eq!(book.tasks().len(), 1, "the same id replaces the record");
        assert!(book.remove(task.id()));
        assert!(book.tasks().is_empty());
        assert!(!book.remove(task.id()));
        let mut book = BatchBook::new();
        book.insert(task);
        assert!(book.validate().is_ok());
        let damaged: BatchBook = serde_json::from_value(serde_json::json!({
            "version": BATCH_FORMAT_VERSION + 1,
            "tasks": [],
        }))
        .expect("the document reads");
        assert!(damaged.validate().is_err());
    }
}
