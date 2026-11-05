//! File transfer tasks (R10, R11).
//!
//! A transfer reports progress and completion only for the task that owns the event
//! ([`TransferId`]), so a late or duplicated message from an earlier attempt can
//! never make a new task look finished.

use serde::{Deserialize, Serialize};

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
        };
        formatter.write_str(message)
    }
}

/// Progress and outcome of one transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferState {
    /// Accepted but not started.
    Queued,
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
    Failed(TransferFailure),
    /// Cancelled by the user; the target was left untouched or the partial file was reported.
    Cancelled,
}

impl TransferState {
    /// Whether the transfer is over and cannot change again.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed(_) | Self::Cancelled
        )
    }

    /// Whether the state claims the target content is complete.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

/// A change reported by the transfer worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferUpdate {
    /// The worker started moving bytes.
    Started,
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
    Failed(TransferFailure),
    /// The user cancelled the transfer.
    Cancelled,
}

/// A transfer update tagged with the task it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEvent {
    /// The task that produced the update.
    pub transfer: TransferId,
    /// What happened.
    pub update: TransferUpdate,
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
            Self::Running { .. } => "running",
            Self::Completed { .. } => "completed",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl TransferUpdate {
    fn label(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Progress { .. } => "progress",
            Self::Completed { .. } => "completion",
            Self::Failed(_) => "failure",
            Self::Cancelled => "cancellation",
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

    /// Current state.
    #[must_use]
    pub fn state(&self) -> &TransferState {
        &self.state
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
        self.apply_update(TransferUpdate::Failed(failure))
    }

    /// Cancels the transfer from any unfinished state.
    ///
    /// # Errors
    ///
    /// Returns [`TransferUpdateError::Finished`] when the transfer already ended.
    pub fn cancel(&mut self) -> Result<(), TransferUpdateError> {
        self.apply_update(TransferUpdate::Cancelled)
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
            (
                TransferState::Queued | TransferState::Running { .. },
                TransferUpdate::Failed(failure),
            ) => {
                self.state = TransferState::Failed(failure.clone());
            }
            (TransferState::Queued | TransferState::Running { .. }, TransferUpdate::Cancelled) => {
                self.state = TransferState::Cancelled;
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

    #[test]
    fn a_new_transfer_is_queued_and_not_successful() {
        let transfer = upload();
        assert_eq!(transfer.state(), &TransferState::Queued);
        assert!(!transfer.state().is_success());
        assert!(!transfer.state().is_finished());
        assert_eq!(transfer.direction().label(), "Upload");
        assert_eq!(transfer.source(), "C:/work/report.txt");
    }

    #[test]
    fn progress_is_monotonic_and_completion_reports_the_byte_count() {
        let mut transfer = upload();
        transfer
            .apply(TransferEvent {
                transfer: transfer.id(),
                update: TransferUpdate::Started,
            })
            .expect("start applies");

        let id = transfer.id();
        transfer
            .apply(TransferEvent {
                transfer: id,
                update: TransferUpdate::Progress {
                    transferred: 4096,
                    total: Some(8192),
                },
            })
            .expect("progress applies");
        assert_eq!(
            transfer.state(),
            &TransferState::Running {
                transferred: 4096,
                total: Some(8192),
            }
        );

        assert_eq!(
            transfer.apply(TransferEvent {
                transfer: id,
                update: TransferUpdate::Progress {
                    transferred: 1024,
                    total: Some(8192),
                },
            }),
            Err(TransferUpdateError::NonMonotonic {
                current: 4096,
                reported: 1024,
            })
        );

        transfer.complete(8192).expect("completion applies");
        assert_eq!(transfer.state(), &TransferState::Completed { bytes: 8192 });
        assert!(transfer.state().is_success());
        assert!(transfer.state().is_finished());
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
        let mut transfer = upload();
        transfer
            .apply(TransferEvent {
                transfer: transfer.id(),
                update: TransferUpdate::Started,
            })
            .expect("start applies");
        transfer.cancel().expect("cancel applies");
        assert_eq!(transfer.state(), &TransferState::Cancelled);

        assert_eq!(
            transfer.complete(10),
            Err(TransferUpdateError::Finished),
            "a cancelled task never reports success"
        );
        assert_eq!(transfer.state(), &TransferState::Cancelled);
    }

    #[test]
    fn failures_explain_themselves_and_cancelled_is_not_success() {
        let mut transfer = upload();
        transfer
            .fail(TransferFailure::NoSpace)
            .expect("failure applies");
        assert_eq!(
            transfer.state(),
            &TransferState::Failed(TransferFailure::NoSpace)
        );
        assert!(!transfer.state().is_success());
        assert_eq!(
            TransferFailure::FinalizeFailed.to_string(),
            "the target could not be replaced; its state must be re-checked"
        );

        let mut cancelled = upload();
        cancelled.cancel().expect("cancel applies");
        assert!(!cancelled.state().is_success());
    }

    #[test]
    fn completion_cannot_claim_fewer_bytes_than_already_transferred() {
        let mut transfer = upload();
        let id = transfer.id();
        transfer
            .apply(TransferEvent {
                transfer: id,
                update: TransferUpdate::Progress {
                    transferred: 900,
                    total: Some(1000),
                },
            })
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
}
