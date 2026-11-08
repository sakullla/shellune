//! File transfer tasks (R10, R11, R17).
//!
//! A transfer reports progress and completion only for the task that owns the event
//! ([`TransferId`]), so a late or duplicated message from an earlier attempt can
//! never make a new task look finished.
//!
//! Three states are deliberately not the same thing, because the user must be able to
//! tell them apart:
//!
//! * [`TransferState::AwaitingOverwrite`] - the target already exists and nothing has
//!   been written yet. Writing waits until the user decides, so cancelling here leaves
//!   the original untouched;
//! * [`TransferState::Completed`] - the final name now refers to the transferred
//!   content. It is only reachable from [`TransferState::Running`], which is what stops
//!   "the temporary file was written" from being reported as a finished transfer;
//! * [`TransferState::Failed`] and [`TransferState::Cancelled`] - the transfer did not
//!   finish. Both carry the leftover path when a partial file could not be removed - or
//!   when the connection ended under a running task, whose worker can no longer report
//!   anything - so a residue is stated instead of being silently pretended away. A
//!   [`ResumeHint`] is attached only when that partial file has a durable checkpoint;
//!   a failure that cannot be checked carries none.
//!
//! [`TransferQueue::retry`] builds a new [`TransferId`]. It carries the failed task's
//! hint when one was recorded, and the worker must still re-check the bytes before it
//! may continue. [`TransferQueue::restart`] never carries a hint: a transfer that
//! cannot resume starts again at zero, and the failed task stays in the list.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::files::{join_path, parent_path};
use crate::ids::{SessionId, TransferId};

/// Which way bytes move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    /// Local file to the remote directory.
    Upload,
    /// Remote file to the local directory.
    Download,
}

impl TransferDirection {
    /// Short label used in the transfer list.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Upload => "Upload",
            Self::Download => "Download",
        }
    }
}

/// The prefix of the file a transfer writes before the target's own name exists.
///
/// It is recognizable on purpose: a leftover of this shape names itself as a partial
/// transfer in a directory listing, without any other tool having to explain it. The
/// constant lives here because the temporary path is part of what a task is: a window
/// whose connection died still has to be able to say which file the task left.
pub const PARTIAL_PREFIX: &str = ".shellune-part-";

/// The temporary path a task writes before the target's own name exists.
///
/// The two directions mean two different file systems, so each is named the way it is
/// written: an upload writes beside its remote target with '/' separators, and a
/// download writes beside its local target with whatever separator this platform uses.
/// Both can be derived from the task alone, which is what lets a report name the
/// leftover of a transfer whose worker never came back.
#[must_use]
pub fn partial_path(direction: TransferDirection, destination: &str, id: TransferId) -> String {
    let name = format!("{PARTIAL_PREFIX}{id}");
    match direction {
        TransferDirection::Upload => match parent_path(destination) {
            Some(parent) if parent != "/" => join_path(&parent, &name),
            Some(_) => format!("/{name}"),
            None => name,
        },
        TransferDirection::Download => {
            let target = Path::new(destination);
            match target.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => {
                    parent.join(&name).to_string_lossy().into_owned()
                }
                _ => name,
            }
        }
    }
}

/// What the user decided about a target that already exists.
///
/// The default is neither: an existing target is never replaced until one of these is
/// chosen, and the transfer stays in [`TransferState::AwaitingOverwrite`] until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverwriteChoice {
    /// Keep the existing target; the transfer ends without writing it.
    KeepExisting,
    /// Replace the existing target with the transferred content.
    Replace,
}

impl OverwriteChoice {
    /// Whether this choice lets the transfer write the target.
    #[must_use]
    pub fn replaces(self) -> bool {
        matches!(self, Self::Replace)
    }
}

/// Why a partial file must not be continued.
///
/// Each case is a reason to transfer again from the start, never a reason to pretend
/// the old offset still counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeRefusal {
    /// The source size, time or bytes are not the ones that were checkpointed.
    SourceChanged,
    /// Identity or the checkpoint itself could not be checked.
    Unverifiable,
    /// The target cannot be opened without truncating, or cannot seek.
    Unsupported,
    /// The partial file is missing, short, or does not hold the checkpointed bytes.
    PartialDamaged,
}

impl std::fmt::Display for ResumeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::SourceChanged => "the source changed, so the partial file was not resumed",
            Self::Unverifiable => "the partial file could not be checked, so it was not resumed",
            Self::Unsupported => {
                "the target cannot resume, so the transfer did not continue from the partial file"
            }
            Self::PartialDamaged => "the partial file is damaged, so it was not resumed",
        };
        formatter.write_str(message)
    }
}

/// Where a confirmed prefix lives, and which checkpoint describes it.
///
/// The hint is not progress. A later attempt may continue only after it has read the
/// source and the partial file again and both still match this checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeHint {
    /// The partial file that holds the confirmed prefix.
    pub partial: String,
    /// The checkpoint record written beside that attempt.
    pub record: String,
    /// Bytes that were flushed and recorded.
    pub offset: u64,
    /// Which checkpoint generation this hint refers to.
    pub generation: u64,
}

/// The checkpoint stored next to a partial file.
///
/// It names the attempt that wrote the prefix, both paths, the source size, whichever
/// of mtime or a full-content hash could be recorded, the hash of the confirmed
/// prefix, the offset and the generation. A resume reads this and then reads the
/// files again; the record alone is not proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialRecord {
    /// The task that first wrote this partial file.
    pub transfer: TransferId,
    /// Which way the bytes were moving.
    pub direction: TransferDirection,
    /// Source path at the time of the checkpoint.
    pub source: String,
    /// Destination path at the time of the checkpoint.
    pub destination: String,
    /// Size of the source when the checkpoint was written.
    pub source_size: u64,
    /// Source mtime, in unix seconds, when the source provided one.
    #[serde(default)]
    pub source_modified_unix: Option<u64>,
    /// SHA-256 of the whole source, lowercase hex, when no mtime was available.
    #[serde(default)]
    pub content_sha256: Option<String>,
    /// SHA-256 of the confirmed prefix, lowercase hex.
    pub prefix_sha256: String,
    /// Bytes covered by [`Self::prefix_sha256`].
    pub confirmed_offset: u64,
    /// Generation of this partial file. A newer file must not reuse an older one.
    pub generation: u64,
}

/// Why a transfer did not complete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferFailure {
    /// The local source file could not be read.
    SourceUnreadable,
    /// The remote path does not exist.
    NotFound,
    /// The remote side refused the operation.
    PermissionDenied,
    /// The remote disk is full.
    NoSpace,
    /// The connection went away during the transfer.
    ConnectionLost,
    /// The transfer was refused because the target existed and overwrite was not confirmed.
    OverwriteNotConfirmed,
    /// The final rename or replacement failed; the target state must be re-checked.
    FinalizeFailed,
    /// The local destination could not be written.
    DestinationUnwritable,
    /// The server refused the operation for a reason of its own, quoted as it gave it.
    Refused {
        /// What the server said, so the reason is specific instead of guessed at.
        reason: String,
    },
    /// A resume was requested and refused before any further byte was written.
    ResumeRefused {
        /// Why the partial file was not continued.
        reason: ResumeRefusal,
    },
}

impl std::fmt::Display for TransferFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::SourceUnreadable => "the source file could not be read",
            Self::NotFound => "the target path does not exist",
            Self::PermissionDenied => "the server refused the operation",
            Self::NoSpace => "the server has no space left",
            Self::ConnectionLost => "the connection was lost during the transfer",
            Self::OverwriteNotConfirmed => {
                "the target already exists and overwrite was not confirmed"
            }
            Self::FinalizeFailed => {
                "the target could not be replaced; its state must be re-checked"
            }
            Self::DestinationUnwritable => "the local destination could not be written",
            Self::Refused { reason } => {
                return write!(formatter, "the server refused the transfer: {reason}");
            }
            Self::ResumeRefused { reason } => return write!(formatter, "{reason}"),
        };
        formatter.write_str(message)
    }
}

impl TransferFailure {
    /// Whether this failure says the connection itself is gone.
    #[must_use]
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::ConnectionLost)
    }
}

/// Progress and outcome of one transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferState {
    /// Accepted but not started.
    Queued,
    /// The target exists and nothing has been written into it yet.
    ///
    /// `total` is the source size when it is known. The transfer is held here until the
    /// user picks an [`OverwriteChoice`]; cancelling from here never touches the target.
    AwaitingOverwrite {
        /// Full size of the source, when known.
        total: Option<u64>,
    },
    /// Running; `total` is `None` when the size is unknown.
    Running {
        /// Bytes written so far.
        transferred: u64,
        /// Full size, when known.
        total: Option<u64>,
    },
    /// Finished: the target exists with the transferred content.
    Completed {
        /// Final byte count.
        bytes: u64,
    },
    /// Did not finish.
    Failed {
        /// Why it stopped.
        failure: TransferFailure,
        /// A partial file left behind: one that could not be removed, or the temporary
        /// file a running task was writing when its connection ended.
        residue: Option<String>,
        /// A checkpoint that may be resumed, only when the partial file was kept for that.
        #[serde(default)]
        resume: Option<ResumeHint>,
    },
    /// Cancelled by the user.
    Cancelled {
        /// A partial file left behind, when it could not be removed or was kept to resume.
        residue: Option<String>,
        /// A checkpoint that may be resumed, only when the partial file was kept for that.
        #[serde(default)]
        resume: Option<ResumeHint>,
    },
}

impl TransferState {
    /// Whether the transfer is over and cannot change again.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }

    /// Whether the state claims the target content is complete.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    /// Whether the transfer is waiting for the user to decide about an existing target.
    #[must_use]
    pub fn is_awaiting_overwrite(&self) -> bool {
        matches!(self, Self::AwaitingOverwrite { .. })
    }

    /// The failure behind this state, when it failed.
    #[must_use]
    pub fn failure(&self) -> Option<&TransferFailure> {
        match self {
            Self::Failed { failure, .. } => Some(failure),
            _ => None,
        }
    }

    /// The partial file that was left behind, when one was.
    ///
    /// A path here means "this is not the target content": it is shown to the user
    /// instead of being hidden, because a partial file nobody mentions is worse than one
    /// that is named. A worker that removed the file it could not keep reports nothing;
    /// a worker that could not remove it names it, and so does a task whose connection
    /// ended while it was writing, because its worker never came back to say anything.
    #[must_use]
    pub fn residue(&self) -> Option<&str> {
        match self {
            Self::Failed { residue, .. } | Self::Cancelled { residue, .. } => residue.as_deref(),
            _ => None,
        }
    }

    /// The checkpoint a later attempt may try to continue, when the partial file was kept.
    #[must_use]
    pub fn resume(&self) -> Option<&ResumeHint> {
        match self {
            Self::Failed { resume, .. } | Self::Cancelled { resume, .. } => resume.as_ref(),
            _ => None,
        }
    }

    /// Bytes written so far, when the transfer reported any.
    #[must_use]
    pub fn transferred(&self) -> Option<u64> {
        match self {
            Self::Running { transferred, .. } => Some(*transferred),
            _ => None,
        }
    }

    /// Full size, when the transfer knows it.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        match self {
            Self::AwaitingOverwrite { total } | Self::Running { total, .. } => *total,
            Self::Completed { bytes } => Some(*bytes),
            Self::Queued | Self::Failed { .. } | Self::Cancelled { .. } => None,
        }
    }

    /// How far the transfer is, in thousandths, when both numbers are known.
    ///
    /// A percentage is a claim about the whole, so it exists only when the total does: a
    /// transfer of unknown size reports `None` and the view prints bytes instead.
    #[must_use]
    pub fn permille(&self) -> Option<u16> {
        let Self::Running { transferred, total } = self else {
            return match self {
                Self::Completed { .. } => Some(1000),
                _ => None,
            };
        };
        let total = (*total)?;
        if total == 0 {
            return Some(1000);
        }
        let scaled = transferred.saturating_mul(1000) / total;
        Some(u16::try_from(scaled.min(1000)).unwrap_or(1000))
    }
}

/// A change reported by the transfer worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferUpdate {
    /// The worker started moving bytes.
    Started,
    /// The target already exists, so the worker stopped before writing it.
    TargetExists {
        /// Full size of the source, when known.
        total: Option<u64>,
    },
    /// The user allowed the existing target to be replaced.
    OverwriteConfirmed,
    /// Bytes were written.
    Progress {
        /// Bytes written so far.
        transferred: u64,
        /// Full size, when known.
        total: Option<u64>,
    },
    /// The target was finalized.
    Completed {
        /// Final byte count.
        bytes: u64,
    },
    /// The transfer failed.
    Failed {
        /// Why it stopped.
        failure: TransferFailure,
        /// A partial file that could not be removed, or that was kept to resume.
        residue: Option<String>,
        /// A checkpoint kept with that partial file, when it is safe to try again.
        #[serde(default)]
        resume: Option<ResumeHint>,
    },
    /// The user cancelled the transfer.
    Cancelled {
        /// A partial file that could not be removed, or that was kept to resume.
        residue: Option<String>,
        /// A checkpoint kept with that partial file, when it is safe to try again.
        #[serde(default)]
        resume: Option<ResumeHint>,
    },
    /// A confirmed prefix was flushed. This is not progress by itself.
    Checkpoint(ResumeHint),
}

/// A transfer update tagged with the task it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEvent {
    /// The task that produced the update.
    pub transfer: TransferId,
    /// What happened.
    pub update: TransferUpdate,
}

impl TransferEvent {
    /// A started event for one task.
    #[must_use]
    pub fn started(transfer: TransferId) -> Self {
        Self {
            transfer,
            update: TransferUpdate::Started,
        }
    }

    /// Progress reported by one task.
    #[must_use]
    pub fn progress(transfer: TransferId, transferred: u64, total: Option<u64>) -> Self {
        Self {
            transfer,
            update: TransferUpdate::Progress { transferred, total },
        }
    }

    /// The news that the target exists and writing waits for a decision.
    #[must_use]
    pub fn target_exists(transfer: TransferId, total: Option<u64>) -> Self {
        Self {
            transfer,
            update: TransferUpdate::TargetExists { total },
        }
    }
}

/// Why a transfer update was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransferUpdateError {
    /// The update arrived for a task that already finished.
    #[error("the transfer already finished")]
    Finished,
    /// The update does not fit the current state.
    #[error("the transfer is {state} and cannot accept {update}")]
    Illegal {
        /// Current state label.
        state: &'static str,
        /// Requested update label.
        update: &'static str,
    },
    /// A progress report went backwards.
    #[error("progress cannot decrease from {current} to {reported} bytes")]
    NonMonotonic {
        /// Bytes already reported.
        current: u64,
        /// Bytes in this report.
        reported: u64,
    },
}

impl TransferState {
    fn label(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::AwaitingOverwrite { .. } => "awaiting the overwrite decision",
            Self::Running { .. } => "running",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Cancelled { .. } => "cancelled",
        }
    }
}

impl TransferUpdate {
    fn label(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::TargetExists { .. } => "an existing target",
            Self::OverwriteConfirmed => "the overwrite confirmation",
            Self::Progress { .. } => "progress",
            Self::Completed { .. } => "completion",
            Self::Failed { .. } => "failure",
            Self::Cancelled { .. } => "cancellation",
            Self::Checkpoint(_) => "a checkpoint",
        }
    }
}

/// One upload or download task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    id: TransferId,
    session: SessionId,
    direction: TransferDirection,
    source: String,
    destination: String,
    state: TransferState,
    /// Last durable checkpoint reported while the task was running.
    ///
    /// A queued retry copies a hint into here so the worker can re-check it. It is not
    /// progress: the state stays queued until the worker reports real bytes.
    #[serde(default)]
    checkpoint: Option<ResumeHint>,
}

impl Transfer {
    /// Creates a queued transfer.
    #[must_use]
    pub fn queued(
        session: SessionId,
        direction: TransferDirection,
        source: impl Into<String>,
        destination: impl Into<String>,
    ) -> Self {
        Self {
            id: TransferId::new(),
            session,
            direction,
            source: source.into(),
            destination: destination.into(),
            state: TransferState::Queued,
            checkpoint: None,
        }
    }

    /// Identifier that tags every event of this task.
    #[must_use]
    pub fn id(&self) -> TransferId {
        self.id
    }

    /// Session the transfer belongs to.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Direction of the transfer.
    #[must_use]
    pub fn direction(&self) -> TransferDirection {
        self.direction
    }

    /// Source path, local or remote depending on the direction.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Destination path, remote or local depending on the direction.
    #[must_use]
    pub fn destination(&self) -> &str {
        &self.destination
    }

    /// The temporary path this task writes before the target's own name exists.
    ///
    /// It is derivable from the task alone, which is why the window can name the file a
    /// task was writing even when the connection ended before the worker could report
    /// anything.
    #[must_use]
    pub fn partial_path(&self) -> String {
        partial_path(self.direction, &self.destination, self.id)
    }

    /// The checkpoint a worker should re-check before continuing, when there is one.
    ///
    /// A running task's checkpoint and a finished task's recorded hint are the same
    /// fact: bytes already confirmed. Neither one is a claim that the target is complete.
    #[must_use]
    pub fn resume_hint(&self) -> Option<&ResumeHint> {
        self.state.resume().or(self.checkpoint.as_ref())
    }

    /// The partial file this task may have left on disk when its connection ended.
    ///
    /// Only a task that was running can have created a temporary file: a queued task
    /// has not been started, and one waiting for an answer about an existing target has
    /// written nothing at all, so neither names a residue. The path is where the bytes
    /// were going, not a file this crate has looked at - the server's disk is not
    /// visible from here - and it is named because a partial file nobody mentions is
    /// the one thing worse than a partial file.
    #[must_use]
    pub fn residue_on_loss(&self) -> Option<String> {
        match self.state {
            TransferState::Running { .. } => Some(self.partial_path()),
            _ => None,
        }
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> &TransferState {
        &self.state
    }

    /// Whether the task is still able to change, which is when it can be cancelled.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.state.is_finished()
    }

    /// Completes the transfer only while running, and only when the byte count is
    /// consistent with the progress already reported.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError`] when the transfer already finished, is not
    /// running, or reports fewer bytes than already transferred.
    pub fn complete(&mut self, bytes: u64) -> Result<(), TransferUpdateError> {
        if let TransferState::Running { transferred, .. } = self.state
            && bytes < transferred
        {
            return Err(TransferUpdateError::NonMonotonic {
                current: transferred,
                reported: bytes,
            });
        }
        self.apply_update(TransferUpdate::Completed { bytes })
    }

    /// Fails the transfer from any unfinished state.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError::Finished`] when the transfer already ended.
    pub fn fail(&mut self, failure: TransferFailure) -> Result<(), TransferUpdateError> {
        self.apply_update(TransferUpdate::Failed {
            failure,
            residue: None,
            resume: None,
        })
    }

    /// Fails the transfer and names the partial file it left behind.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError::Finished`] when the transfer already ended.
    pub fn fail_with(
        &mut self,
        failure: TransferFailure,
        residue: Option<String>,
        resume: Option<ResumeHint>,
    ) -> Result<(), TransferUpdateError> {
        self.apply_update(TransferUpdate::Failed {
            failure,
            residue,
            resume,
        })
    }

    /// Cancels the transfer from any unfinished state.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError::Finished`] when the transfer already ended.
    pub fn cancel(&mut self) -> Result<(), TransferUpdateError> {
        self.apply_update(TransferUpdate::Cancelled {
            residue: None,
            resume: None,
        })
    }

    /// Cancels the transfer and names the partial file it left behind.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError::Finished`] when the transfer already ended.
    pub fn cancel_with(
        &mut self,
        residue: Option<String>,
        resume: Option<ResumeHint>,
    ) -> Result<(), TransferUpdateError> {
        self.apply_update(TransferUpdate::Cancelled { residue, resume })
    }

    /// Applies a worker update if it belongs to this task.
    ///
    /// Returns `false` for an event that carries another [`TransferId`], which is how
    /// a late message from a previous attempt is dropped instead of overwriting the
    /// state of a newer task.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError`] when the update belongs to this task but does
    /// not fit its current state.
    pub fn apply(&mut self, event: TransferEvent) -> Result<bool, TransferUpdateError> {
        if event.transfer != self.id {
            return Ok(false);
        }
        self.apply_update(event.update)?;
        Ok(true)
    }

    fn apply_update(&mut self, update: TransferUpdate) -> Result<(), TransferUpdateError> {
        match (&self.state, &update) {
            (TransferState::Queued, TransferUpdate::Started) => {
                self.state = TransferState::Running {
                    transferred: 0,
                    total: None,
                };
            }
            (TransferState::Queued, TransferUpdate::Progress { transferred, total }) => {
                self.state = TransferState::Running {
                    transferred: *transferred,
                    total: *total,
                };
            }
            (
                TransferState::Queued | TransferState::AwaitingOverwrite { .. },
                TransferUpdate::TargetExists { total },
            ) => {
                self.state = TransferState::AwaitingOverwrite { total: *total };
            }
            (TransferState::AwaitingOverwrite { total }, TransferUpdate::OverwriteConfirmed) => {
                // Nothing was written while the question was open, so the transfer
                // starts at zero bytes with the size it already knew.
                self.state = TransferState::Running {
                    transferred: 0,
                    total: *total,
                };
            }
            (TransferState::AwaitingOverwrite { .. }, TransferUpdate::Started) => {
                self.state = TransferState::Running {
                    transferred: 0,
                    total: None,
                };
            }
            (
                TransferState::Running { transferred, .. },
                TransferUpdate::Progress {
                    transferred: reported,
                    total,
                },
            ) => {
                if reported < transferred {
                    return Err(TransferUpdateError::NonMonotonic {
                        current: *transferred,
                        reported: *reported,
                    });
                }
                self.state = TransferState::Running {
                    transferred: *reported,
                    total: total.or(*total),
                };
            }
            (TransferState::Running { .. }, TransferUpdate::Completed { bytes }) => {
                self.state = TransferState::Completed { bytes: *bytes };
            }
            (TransferState::Running { .. }, TransferUpdate::Checkpoint(hint)) => {
                // A checkpoint names bytes already flushed. It does not move the
                // progress number; the next progress report does that.
                if hint.offset > 0 {
                    self.checkpoint = Some(hint.clone());
                }
            }
            (
                TransferState::Queued
                | TransferState::AwaitingOverwrite { .. }
                | TransferState::Running { .. },
                TransferUpdate::Failed {
                    failure,
                    residue,
                    resume,
                },
            ) => {
                if let Some(hint) = resume {
                    self.checkpoint = Some(hint.clone());
                }
                self.state = TransferState::Failed {
                    failure: failure.clone(),
                    residue: residue.clone(),
                    resume: resume.clone(),
                };
            }
            (
                TransferState::Queued
                | TransferState::AwaitingOverwrite { .. }
                | TransferState::Running { .. },
                TransferUpdate::Cancelled { residue, resume },
            ) => {
                if let Some(hint) = resume {
                    self.checkpoint = Some(hint.clone());
                }
                self.state = TransferState::Cancelled {
                    residue: residue.clone(),
                    resume: resume.clone(),
                };
            }
            // A cancellation the window already reported can still learn which partial
            // file survived it: the outcome does not change, only the detail, and a
            // residue the user is never told about would be the one thing worse than the
            // file itself. A checkpoint arrives the same way, and only then.
            (
                TransferState::Cancelled {
                    residue: previous,
                    resume: None,
                },
                TransferUpdate::Cancelled { residue, resume },
            ) if residue.is_some() || resume.is_some() => {
                if let Some(hint) = resume {
                    self.checkpoint = Some(hint.clone());
                }
                self.state = TransferState::Cancelled {
                    residue: residue.clone().or_else(|| previous.clone()),
                    resume: resume.clone(),
                };
            }
            (state, _) if state.is_finished() => {
                return Err(TransferUpdateError::Finished);
            }
            (state, update) => {
                return Err(TransferUpdateError::Illegal {
                    state: state.label(),
                    update: update.label(),
                });
            }
        }
        Ok(())
    }
}

/// Every transfer task the window knows about, in the order they were started.
///
/// The queue is the single place that decides which task an event belongs to, which
/// tasks a session owns and which of them are still running, so a session that closes
/// cancels exactly its own work and nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferQueue {
    transfers: Vec<Transfer>,
}

impl TransferQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a task and returns the identifier its events must carry.
    pub fn add(&mut self, transfer: Transfer) -> TransferId {
        let id = transfer.id();
        self.transfers.push(transfer);
        id
    }

    /// Every task, oldest first.
    #[must_use]
    pub fn transfers(&self) -> &[Transfer] {
        &self.transfers
    }

    /// One task, by identifier.
    #[must_use]
    pub fn get(&self, id: TransferId) -> Option<&Transfer> {
        self.transfers.iter().find(|transfer| transfer.id() == id)
    }

    /// Whether this queue still holds a task with this identifier.
    #[must_use]
    pub fn contains(&self, id: TransferId) -> bool {
        self.get(id).is_some()
    }

    /// Tasks that belong to one session, active or not.
    pub fn for_session(&self, session: SessionId) -> impl Iterator<Item = &Transfer> {
        self.transfers
            .iter()
            .filter(move |transfer| transfer.session() == session)
    }

    /// How many tasks of one session are still able to change.
    #[must_use]
    pub fn active_count(&self, session: SessionId) -> usize {
        self.for_session(session)
            .filter(|transfer| transfer.is_active())
            .count()
    }

    /// Tasks that are still able to change, oldest first.
    #[must_use]
    pub fn active(&self) -> Vec<&Transfer> {
        self.transfers
            .iter()
            .filter(|transfer| transfer.is_active())
            .collect()
    }

    /// Tasks waiting for the user to decide about an existing target.
    #[must_use]
    pub fn awaiting_overwrite(&self) -> Vec<&Transfer> {
        self.transfers
            .iter()
            .filter(|transfer| transfer.state().is_awaiting_overwrite())
            .collect()
    }

    /// Applies an event to the task it belongs to.
    ///
    /// Returns the identifier of the task that changed, or `None` when no task in this
    /// queue owns the event - which is what a late message from a closed or replaced
    /// task looks like, and why it cannot touch anything else.
    pub fn apply(&mut self, event: &TransferEvent) -> Option<TransferId> {
        let transfer = self
            .transfers
            .iter_mut()
            .find(|transfer| transfer.id() == event.transfer)?;
        let applied = transfer.apply(event.clone()).unwrap_or(false);
        applied.then_some(transfer.id())
    }

    /// Cancels one task, returning whether it was still active.
    ///
    /// The state is set here so the list cannot keep showing a running task whose
    /// worker was told to stop; the worker's own cancellation event then carries the
    /// residue, when it found any.
    pub fn cancel(&mut self, id: TransferId) -> bool {
        let Some(transfer) = self.transfers.iter_mut().find(|task| task.id() == id) else {
            return false;
        };
        if !transfer.is_active() {
            return false;
        }
        transfer.cancel().is_ok()
    }

    /// Cancels every active task of one session, returning the identifiers to tell.
    ///
    /// This is what closing a session does: the tasks of that session end, and the
    /// tasks of every other session keep running. A checkpoint the worker already
    /// reported stays on the task, because the connection is about to go away and
    /// cannot be relied on to send that fact again. A task with no checkpoint is
    /// cancelled without one, and does not invent an offset.
    pub fn cancel_session(&mut self, session: SessionId) -> Vec<TransferId> {
        self.transfers
            .iter_mut()
            .filter(|transfer| transfer.session() == session && transfer.is_active())
            .filter_map(|transfer| {
                let id = transfer.id();
                let resume = transfer.checkpoint.clone().filter(|hint| hint.offset > 0);
                let residue = resume.as_ref().map(|hint| hint.partial.clone());
                transfer.cancel_with(residue, resume).ok().map(|()| id)
            })
            .collect()
    }

    /// Reports every active task of a session as failed, returning their identifiers.
    ///
    /// A connection that ended cannot leave a task claiming it is still moving bytes,
    /// so the tasks of that session end with it and say why. A task that was running
    /// also names the temporary file it was writing ([Transfer::residue_on_loss]): its
    /// worker died with the connection and cannot report it, and a partial file the user
    /// is never told about outlives the session that produced it.
    pub fn fail_session(
        &mut self,
        session: SessionId,
        failure: TransferFailure,
    ) -> Vec<TransferId> {
        self.transfers
            .iter_mut()
            .filter(|transfer| transfer.session() == session && transfer.is_active())
            .filter_map(|transfer| {
                let id = transfer.id();
                let residue = transfer.residue_on_loss();
                // The worker died with the connection, so the last checkpoint it managed
                // to report is the only resume fact left. A task that never checkpointed
                // names its partial file and does not pretend that file can be continued.
                let resume = residue
                    .as_ref()
                    .and_then(|_| transfer.checkpoint.clone())
                    .filter(|hint| hint.offset > 0);
                transfer
                    .fail_with(failure.clone(), residue, resume)
                    .ok()
                    .map(|()| id)
            })
            .collect()
    }

    /// Starts a new task for the same two paths.
    ///
    /// The returned task has its own [`TransferId`] and stays queued. When the failed
    /// task recorded a checkpoint, the new task carries that hint so the worker can
    /// re-check it; the hint is not progress. Without a hint the new task has nothing
    /// to continue, which is a transfer from the first byte.
    #[must_use]
    pub fn retry(&self, id: TransferId) -> Option<Transfer> {
        self.get(id).map(|failed| {
            let mut next = Transfer::queued(
                failed.session(),
                failed.direction(),
                failed.source(),
                failed.destination(),
            );
            next.checkpoint = failed.state().resume().cloned();
            next
        })
    }

    /// Starts a new task from the first byte, ignoring any checkpoint.
    ///
    /// This is the explicit re-transfer: the previous failure stays in the queue, and
    /// the new task must not display the old offset.
    #[must_use]
    pub fn restart(&self, id: TransferId) -> Option<Transfer> {
        self.get(id)
            .filter(|failed| !failed.state().is_success())
            .map(|failed| {
                Transfer::queued(
                    failed.session(),
                    failed.direction(),
                    failed.source(),
                    failed.destination(),
                )
            })
    }

    /// Drops one task from the list, which is what the view's own dismissal does.
    pub fn remove(&mut self, id: TransferId) -> bool {
        let before = self.transfers.len();
        self.transfers.retain(|transfer| transfer.id() != id);
        self.transfers.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload() -> Transfer {
        Transfer::queued(
            SessionId::new(),
            TransferDirection::Upload,
            "C:/work/report.txt",
            "/home/deploy/report.txt",
        )
    }

    fn started(mut transfer: Transfer) -> Transfer {
        let id = transfer.id();
        transfer
            .apply(TransferEvent::started(id))
            .expect("start applies");
        transfer
    }

    #[test]
    fn a_new_transfer_is_queued_and_not_successful() {
        let transfer = upload();
        assert_eq!(transfer.state(), &TransferState::Queued);
        assert!(!transfer.state().is_success());
        assert!(!transfer.state().is_finished());
        assert!(transfer.is_active());
        assert_eq!(transfer.direction().label(), "Upload");
        assert_eq!(transfer.source(), "C:/work/report.txt");
    }

    #[test]
    fn progress_is_monotonic_and_completion_reports_the_byte_count() {
        let mut transfer = upload();
        let id = transfer.id();
        transfer
            .apply(TransferEvent::started(id))
            .expect("start applies");
        transfer
            .apply(TransferEvent::progress(id, 4096, Some(8192)))
            .expect("progress applies");
        assert_eq!(
            transfer.state(),
            &TransferState::Running {
                transferred: 4096,
                total: Some(8192),
            }
        );
        assert_eq!(transfer.state().permille(), Some(500));

        assert_eq!(
            transfer.apply(TransferEvent::progress(id, 1024, Some(8192))),
            Err(TransferUpdateError::NonMonotonic {
                current: 4096,
                reported: 1024,
            })
        );

        transfer.complete(8192).expect("completion applies");
        assert_eq!(transfer.state(), &TransferState::Completed { bytes: 8192 });
        assert!(transfer.state().is_success());
        assert!(transfer.state().is_finished());
        assert_eq!(transfer.state().permille(), Some(1000));
    }

    #[test]
    fn an_existing_target_holds_the_transfer_until_the_user_decides() {
        let mut transfer = upload();
        let id = transfer.id();
        transfer
            .apply(TransferEvent::target_exists(id, Some(2048)))
            .expect("the existing target applies");
        assert_eq!(
            transfer.state(),
            &TransferState::AwaitingOverwrite { total: Some(2048) }
        );
        assert!(transfer.state().is_awaiting_overwrite());
        assert!(!transfer.state().is_success());
        assert!(!transfer.state().is_finished());
        assert_eq!(
            transfer.state().transferred(),
            None,
            "nothing was written while the question was open"
        );

        // Writing the temporary file is not the same as finishing: a transfer that is
        // waiting for the decision cannot jump straight to completed.
        assert_eq!(
            transfer.complete(2048),
            Err(TransferUpdateError::Illegal {
                state: "awaiting the overwrite decision",
                update: "completion",
            })
        );
        assert!(!transfer.state().is_success());

        transfer
            .apply(TransferEvent {
                transfer: id,
                update: TransferUpdate::OverwriteConfirmed,
            })
            .expect("the confirmation applies");
        assert_eq!(
            transfer.state(),
            &TransferState::Running {
                transferred: 0,
                total: Some(2048),
            }
        );
        transfer.complete(2048).expect("completion applies");
        assert!(transfer.state().is_success());
    }

    #[test]
    fn cancelling_while_asking_about_an_existing_target_is_not_a_success() {
        let mut transfer = upload();
        let id = transfer.id();
        transfer
            .apply(TransferEvent::target_exists(id, Some(10)))
            .expect("the question applies");
        transfer.cancel().expect("cancel applies");
        assert_eq!(
            transfer.state(),
            &TransferState::Cancelled {
                residue: None,
                resume: None,
            }
        );
        assert!(!transfer.state().is_success());
        assert_eq!(
            transfer.complete(10),
            Err(TransferUpdateError::Finished),
            "a cancelled transfer never reports success"
        );
    }

    #[test]
    fn a_late_event_from_a_previous_attempt_is_ignored() {
        let stale = upload();
        let stale_id = stale.id();
        let mut current = upload();

        let applied = current
            .apply(TransferEvent {
                transfer: stale_id,
                update: TransferUpdate::Completed { bytes: 4096 },
            })
            .expect("a foreign event is not an error");

        assert!(!applied);
        assert_eq!(current.state(), &TransferState::Queued);
    }

    #[test]
    fn a_stale_attempt_cannot_mark_an_already_cancelled_task_complete() {
        let mut transfer = started(upload());
        transfer.cancel().expect("cancel applies");
        assert_eq!(
            transfer.state(),
            &TransferState::Cancelled {
                residue: None,
                resume: None,
            }
        );

        assert_eq!(
            transfer.complete(10),
            Err(TransferUpdateError::Finished),
            "a cancelled task never reports success"
        );
        assert_eq!(
            transfer.state(),
            &TransferState::Cancelled {
                residue: None,
                resume: None,
            }
        );
    }

    #[test]
    fn failures_and_cancellations_name_the_partial_file_they_left() {
        let mut transfer = started(upload());
        transfer
            .fail_with(
                TransferFailure::FinalizeFailed,
                Some("/home/deploy/.shellune-part-7".to_owned()),
                None,
            )
            .expect("failure applies");
        assert_eq!(
            transfer.state().failure(),
            Some(&TransferFailure::FinalizeFailed)
        );
        assert_eq!(
            transfer.state().residue(),
            Some("/home/deploy/.shellune-part-7"),
            "a partial file that could not be removed is named, not hidden"
        );
        assert!(!transfer.state().is_success());
        assert_eq!(
            TransferFailure::FinalizeFailed.to_string(),
            "the target could not be replaced; its state must be re-checked"
        );

        let mut cancelled = started(upload());
        cancelled
            .cancel_with(Some("C:/work/.shellune-part-3".to_owned()), None)
            .expect("cancel applies");
        assert_eq!(
            cancelled.state().residue(),
            Some("C:/work/.shellune-part-3")
        );
        assert!(!cancelled.state().is_success());
    }

    #[test]
    fn completion_cannot_claim_fewer_bytes_than_already_transferred() {
        let mut transfer = upload();
        let id = transfer.id();
        transfer
            .apply(TransferEvent::progress(id, 900, Some(1000)))
            .expect("progress applies");

        assert_eq!(
            transfer.complete(500),
            Err(TransferUpdateError::NonMonotonic {
                current: 900,
                reported: 500,
            })
        );
        assert!(!transfer.state().is_success());
    }

    #[test]
    fn a_queue_drops_events_of_tasks_it_never_held() {
        let mut queue = TransferQueue::new();
        let transfer = upload();
        let id = queue.add(transfer);
        assert!(queue.contains(id));

        let stranger = upload();
        assert_eq!(
            queue.apply(&TransferEvent::started(stranger.id())),
            None,
            "an event for an unknown task changes nothing"
        );
        assert_eq!(queue.apply(&TransferEvent::started(id)), Some(id));
        assert!(queue.get(id).is_some_and(Transfer::is_active));
    }

    #[test]
    fn closing_one_session_cancels_only_its_own_tasks() {
        let first = SessionId::new();
        let second = SessionId::new();
        let mut queue = TransferQueue::new();
        let first_upload = queue.add(Transfer::queued(
            first,
            TransferDirection::Upload,
            "a.txt",
            "/home/deploy/a.txt",
        ));
        let first_download = queue.add(Transfer::queued(
            first,
            TransferDirection::Download,
            "/home/deploy/b.txt",
            "C:/work/b.txt",
        ));
        let second_upload = queue.add(Transfer::queued(
            second,
            TransferDirection::Upload,
            "c.txt",
            "/home/deploy/c.txt",
        ));
        assert_eq!(queue.active_count(first), 2);
        assert_eq!(queue.active_count(second), 1);

        let cancelled = queue.cancel_session(first);
        assert_eq!(cancelled, vec![first_upload, first_download]);
        assert_eq!(queue.active_count(first), 0);
        assert_eq!(
            queue.active_count(second),
            1,
            "another session's transfer keeps running"
        );
        assert!(queue.get(second_upload).is_some_and(Transfer::is_active));
    }

    #[test]
    fn a_lost_connection_fails_only_the_tasks_of_that_session() {
        let first = SessionId::new();
        let second = SessionId::new();
        let mut queue = TransferQueue::new();
        let lost = queue.add(Transfer::queued(
            first,
            TransferDirection::Upload,
            "a.txt",
            "/home/deploy/a.txt",
        ));
        let kept = queue.add(Transfer::queued(
            second,
            TransferDirection::Download,
            "/home/deploy/b.txt",
            "C:/work/b.txt",
        ));

        assert_eq!(
            queue.fail_session(first, TransferFailure::ConnectionLost),
            vec![lost]
        );
        assert_eq!(
            queue.get(lost).and_then(|task| task.state().failure()),
            Some(&TransferFailure::ConnectionLost)
        );
        assert!(queue.get(kept).is_some_and(Transfer::is_active));
    }

    #[test]
    fn a_retry_is_a_new_task_and_never_a_continuation() {
        let session = SessionId::new();
        let mut queue = TransferQueue::new();
        let failed = queue.add(Transfer::queued(
            session,
            TransferDirection::Upload,
            "C:/work/报告 终稿.txt",
            "/home/deploy/报告 终稿.txt",
        ));
        let id = failed;
        let mut transfer = queue.get(id).cloned().expect("the task exists");
        transfer
            .apply(TransferEvent::progress(id, 4096, Some(8192)))
            .expect("progress applies");
        transfer
            .fail(TransferFailure::ConnectionLost)
            .expect("failure applies");
        *queue
            .transfers
            .iter_mut()
            .find(|task| task.id() == id)
            .expect("the task is queued") = transfer;

        let retried = queue.retry(id).expect("a failed task can be retried");
        assert_ne!(retried.id(), id, "a retry is a new task");
        assert_eq!(retried.state(), &TransferState::Queued);
        assert_eq!(
            retried.source(),
            "C:/work/报告 终稿.txt",
            "the paths keep their exact characters"
        );
        assert_eq!(retried.destination(), "/home/deploy/报告 终稿.txt");

        queue.add(retried.clone());
        assert!(queue.get(id).is_some(), "the failed task stays readable");
    }

    #[test]
    fn a_finished_task_cannot_be_cancelled_again() {
        let session = SessionId::new();
        let mut queue = TransferQueue::new();
        let id = queue.add(Transfer::queued(
            session,
            TransferDirection::Upload,
            "a.txt",
            "/home/deploy/a.txt",
        ));
        queue
            .apply(&TransferEvent {
                transfer: id,
                update: TransferUpdate::Progress {
                    transferred: 10,
                    total: Some(10),
                },
            })
            .expect("progress applies");
        queue
            .apply(&TransferEvent {
                transfer: id,
                update: TransferUpdate::Completed { bytes: 10 },
            })
            .expect("completion applies");

        assert!(
            !queue.cancel(id),
            "a completed transfer is not cancelled into failure"
        );
        assert!(
            queue.get(id).is_some_and(|task| task.state().is_success()),
            "the completed state survives"
        );
        assert!(queue.active().is_empty());
    }

    #[test]
    fn a_temporary_name_is_derivable_from_the_task_alone() {
        let id = TransferId::from_uuid(uuid::Uuid::nil());
        assert_eq!(
            partial_path(TransferDirection::Upload, "/home/deploy/report.txt", id),
            format!("/home/deploy/{PARTIAL_PREFIX}{id}"),
            "an upload writes beside its remote target"
        );
        assert_eq!(
            partial_path(TransferDirection::Upload, "/report.txt", id),
            format!("/{PARTIAL_PREFIX}{id}"),
            "a target in the root keeps a single separator"
        );
        assert_eq!(
            partial_path(TransferDirection::Upload, "report.txt", id),
            format!("/{PARTIAL_PREFIX}{id}"),
            "a remote target is an absolute path, so a bare name is read as one in the root"
        );

        let download = partial_path(
            TransferDirection::Download,
            "/home/deploy/报告 终稿.txt",
            id,
        );
        assert_eq!(
            download,
            Path::new("/home/deploy")
                .join(format!("{PARTIAL_PREFIX}{id}"))
                .to_string_lossy()
                .into_owned(),
            "a download writes beside its local target, as this platform writes paths"
        );
        assert!(
            !download.contains("报告"),
            "the temporary file cannot be mistaken for the target: {download}"
        );
        assert_eq!(
            partial_path(TransferDirection::Download, "report.txt", id),
            format!("{PARTIAL_PREFIX}{id}"),
            "a target with no directory is written beside the working directory"
        );
    }

    #[test]
    fn a_task_names_its_temporary_file_the_same_way_wherever_it_is_asked() {
        let task = upload();
        assert_eq!(
            task.partial_path(),
            partial_path(task.direction(), task.destination(), task.id())
        );
        assert_eq!(task.residue_on_loss(), None, "a queued task wrote nothing");

        let id = task.id();
        let mut running = started(upload());
        running
            .apply(TransferEvent::progress(id, 1024, Some(4096)))
            .expect("progress applies");
        assert_eq!(
            running.residue_on_loss(),
            Some(running.partial_path()),
            "a running task was writing that file, so it is named"
        );

        let mut waiting = upload();
        waiting
            .apply(TransferEvent::target_exists(waiting.id(), Some(10)))
            .expect("the question applies");
        assert_eq!(
            waiting.residue_on_loss(),
            None,
            "nothing is written while the overwrite question is open"
        );
    }

    #[test]
    fn a_connection_that_ends_names_the_partial_file_of_its_running_tasks_only() {
        let session = SessionId::new();
        let mut queue = TransferQueue::new();
        let running = Transfer::queued(
            session,
            TransferDirection::Upload,
            "C:/work/report.txt",
            "/home/deploy/report.txt",
        );
        let running_id = queue.add(running.clone());
        queue
            .apply(&TransferEvent::progress(running_id, 512, Some(1024)))
            .expect("progress applies");
        let queued_id = queue.add(Transfer::queued(
            session,
            TransferDirection::Download,
            "/home/deploy/other.txt",
            "C:/work/other.txt",
        ));

        let failed = queue.fail_session(session, TransferFailure::ConnectionLost);
        assert_eq!(failed.len(), 2, "every active task of the session ends");

        let partial = queue
            .get(running_id)
            .expect("the task is listed")
            .partial_path();
        assert_eq!(
            partial,
            format!("/home/deploy/{PARTIAL_PREFIX}{running_id}")
        );
        assert_eq!(
            queue
                .get(running_id)
                .expect("the task is listed")
                .state()
                .residue(),
            Some(partial.as_str()),
            "the file the bytes were going into is named, not hidden"
        );
        assert_eq!(
            queue.get(running_id).map(|task| task.state().failure()),
            Some(Some(&TransferFailure::ConnectionLost)),
            "and the task is reported as the failure it was"
        );
        assert_eq!(
            queue
                .get(queued_id)
                .expect("the task is listed")
                .state()
                .residue(),
            None,
            "a task that never ran left no partial file to name"
        );
    }

    fn hint(offset: u64) -> ResumeHint {
        ResumeHint {
            partial: format!("/home/deploy/{PARTIAL_PREFIX}kept"),
            record: "C:/temp/kept.json".to_owned(),
            offset,
            generation: 1,
        }
    }

    #[test]
    fn a_checkpoint_is_kept_when_the_connection_ends_and_is_not_progress() {
        let session = SessionId::new();
        let mut queue = TransferQueue::new();
        let id = queue.add(Transfer::queued(
            session,
            TransferDirection::Upload,
            "C:/work/report.txt",
            "/home/deploy/report.txt",
        ));
        queue
            .apply(&TransferEvent::progress(id, 1024, Some(4096)))
            .expect("progress applies");
        queue
            .apply(&TransferEvent {
                transfer: id,
                update: TransferUpdate::Checkpoint(hint(1024)),
            })
            .expect("the checkpoint applies");
        assert_eq!(
            queue.get(id).and_then(|task| task.state().transferred()),
            Some(1024),
            "a checkpoint does not invent a second progress number"
        );

        queue.fail_session(session, TransferFailure::ConnectionLost);
        let failed = queue.get(id).expect("the task stays listed");
        assert!(!failed.state().is_success());
        assert_eq!(failed.state().resume(), Some(&hint(1024)));
        assert_eq!(failed.resume_hint().map(|item| item.offset), Some(1024));

        let resumed = queue.retry(id).expect("the failure can be retried");
        assert_ne!(resumed.id(), id);
        assert_eq!(resumed.state(), &TransferState::Queued);
        assert_eq!(
            resumed.state().transferred(),
            None,
            "carrying a hint does not display the old offset as progress"
        );
        assert_eq!(resumed.resume_hint(), Some(&hint(1024)));

        let fresh = queue.restart(id).expect("the same failure can start over");
        assert!(fresh.resume_hint().is_none());
        assert_eq!(fresh.state(), &TransferState::Queued);
        assert!(queue.get(id).is_some(), "the failed task stays readable");
    }

    #[test]
    fn closing_a_session_keeps_that_sessions_checkpoint_only() {
        let first = SessionId::new();
        let second = SessionId::new();
        let mut queue = TransferQueue::new();
        let running = queue.add(Transfer::queued(
            first,
            TransferDirection::Upload,
            "C:/work/report.txt",
            "/home/deploy/report.txt",
        ));
        let waiting = queue.add(Transfer::queued(
            first,
            TransferDirection::Upload,
            "C:/work/other.txt",
            "/home/deploy/other.txt",
        ));
        let other = queue.add(Transfer::queued(
            second,
            TransferDirection::Download,
            "/home/deploy/keep.bin",
            "C:/work/keep.bin",
        ));
        queue
            .apply(&TransferEvent::progress(running, 1024, Some(4096)))
            .expect("progress applies");
        queue
            .apply(&TransferEvent {
                transfer: running,
                update: TransferUpdate::Checkpoint(hint(1024)),
            })
            .expect("the checkpoint applies");
        queue
            .apply(&TransferEvent::progress(other, 64, Some(128)))
            .expect("the other session reports its own progress");

        let stopped = queue.cancel_session(first);
        assert_eq!(stopped, vec![running, waiting]);
        let closed = queue.get(running).expect("the task stays listed");
        assert!(!closed.state().is_success());
        assert_eq!(closed.state().resume(), Some(&hint(1024)));
        assert_eq!(
            closed.state().residue(),
            Some(hint(1024).partial.as_str()),
            "the confirmed partial file is named"
        );
        assert!(
            queue
                .get(waiting)
                .is_some_and(|task| task.state().resume().is_none() && !task.state().is_success()),
            "a task that never checkpointed does not invent an offset"
        );
        assert_eq!(
            queue.get(other).and_then(|task| task.state().transferred()),
            Some(64),
            "the other session's transfer is still the progress it reported"
        );
        assert_eq!(queue.active_count(second), 1);
        assert_eq!(queue.active_count(first), 0);
    }

    #[test]
    fn a_resume_that_is_refused_is_not_a_completed_transfer() {
        let mut transfer = upload();
        let id = transfer.id();
        transfer
            .apply(TransferEvent {
                transfer: id,
                update: TransferUpdate::Failed {
                    failure: TransferFailure::ResumeRefused {
                        reason: ResumeRefusal::SourceChanged,
                    },
                    residue: None,
                    resume: None,
                },
            })
            .expect("the refusal applies");
        assert!(!transfer.state().is_success());
        assert!(transfer.state().resume().is_none());
        assert_eq!(
            transfer.state().failure().map(ToString::to_string),
            Some("the source changed, so the partial file was not resumed".to_owned())
        );
        assert_eq!(
            transfer.complete(10),
            Err(TransferUpdateError::Finished),
            "a refused resume cannot be completed afterwards"
        );
    }

    #[test]
    fn a_cancellation_can_learn_the_partial_file_it_kept() {
        let mut transfer = started(upload());
        transfer.cancel().expect("the window cancels at once");
        let id = transfer.id();
        transfer
            .apply(TransferEvent {
                transfer: id,
                update: TransferUpdate::Cancelled {
                    residue: Some("/home/deploy/.shellune-part-kept".to_owned()),
                    resume: Some(hint(512)),
                },
            })
            .expect("the worker's residue applies");
        assert_eq!(
            transfer.state().residue(),
            Some("/home/deploy/.shellune-part-kept")
        );
        assert_eq!(transfer.state().resume().map(|item| item.offset), Some(512));
        assert!(!transfer.state().is_success());
    }
}
