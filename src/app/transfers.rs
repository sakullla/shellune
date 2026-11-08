//! Transfers of one window: what was asked for, how far it is and what it became (R10, R17).
//!
//! The rules that decide what a transfer may claim live in shellune-core
//! ([`shellune_core::TransferQueue`]); the worker that moves bytes lives in
//! shellune-ssh. This module is the wiring between them and the window: it owns the
//! queue, the link to each session's connection and the questions the user still has to
//! answer.
//!
//! Three things follow from that split:
//!
//! * an event is applied only when its [`shellune_core::TransferId`] is in the queue, so
//!   a message from a closed session or from an attempt that already ended cannot change
//!   what the list says;
//! * a session that closes cancels exactly its own tasks and nothing else, because the
//!   queue answers which tasks a session owns;
//! * a retry starts a new task from the same two paths. It carries a checkpoint only
//!   when the failed task recorded one; the worker still has to re-check the bytes,
//!   and a restart never carries the checkpoint. The failed task stays readable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shellune_core::{
    OverwriteChoice, SessionId, Transfer, TransferDirection, TransferEvent, TransferFailure,
    TransferId, TransferQueue, join_path,
};
use shellune_ssh::{TransferCommand, TransferLink, TransferRequest};

/// Where a transfer view sends its commands and reads the events.
///
/// The real implementation is the session's connection ([TransferLink]); a test supplies
/// its own, which is what makes the view's behaviour observable without a server.
pub trait TransferChannel {
    /// Sends a command, returning whether it left this process.
    fn send(&mut self, command: TransferCommand) -> bool;
    /// Every event that arrived since the last call.
    fn drain(&mut self) -> Vec<TransferEvent>;
}

impl TransferChannel for TransferLink {
    fn send(&mut self, command: TransferCommand) -> bool {
        TransferLink::send(self, command)
    }

    fn drain(&mut self) -> Vec<TransferEvent> {
        TransferLink::drain(self)
    }
}

/// Why a transfer was not started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferProblem {
    /// The local path names no file: it is empty or ends at a directory separator.
    SourceNamesNoFile,
    /// No directory is open, so there is nowhere for the file to go.
    DirectoryUnknown,
    /// The connection cannot take the command, so no task was created.
    CommandRefused,
}

/// Why a download or upload could not be offered at all.
///
/// It is separate from [`TransferProblem`] because a path this window can refuse on its
/// own never becomes a task: a transfer that was never started has no failure to show in
/// the list, only a reason beside the field the user typed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathProblem {
    /// The path is empty.
    Empty,
    /// The path names no file, only a directory or nothing at all.
    NoName,
}

/// The transfers of every session this window can move files on.
#[derive(Default)]
pub struct TransferViews {
    channels: HashMap<SessionId, Box<dyn TransferChannel>>,
    queue: TransferQueue,
}

impl TransferViews {
    /// No transfers and no connection that could run one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a session's connection can move files.
    pub fn attach(&mut self, session: SessionId, channel: Box<dyn TransferChannel>) {
        self.channels.insert(session, channel);
    }

    /// Whether this session can move files at all.
    #[must_use]
    pub fn can_transfer(&self, session: SessionId) -> bool {
        self.channels.contains_key(&session)
    }

    /// Every task, oldest first, for the transfers page.
    #[must_use]
    pub fn transfers(&self) -> &[Transfer] {
        self.queue.transfers()
    }

    /// Tasks of one session, oldest first, for that session's own list.
    pub fn for_session(&self, session: SessionId) -> impl Iterator<Item = &Transfer> {
        self.queue.for_session(session)
    }

    /// One task, by identifier.
    #[cfg(test)]
    #[must_use]
    pub fn get(&self, id: TransferId) -> Option<&Transfer> {
        self.queue.get(id)
    }

    /// How many tasks of one session are still running or waiting for an answer.
    #[must_use]
    pub fn active_count(&self, session: SessionId) -> usize {
        self.queue.active_count(session)
    }

    /// Tasks whose target exists and that wait for the user's decision.
    #[cfg(test)]
    #[must_use]
    pub fn awaiting_overwrite(&self) -> Vec<&Transfer> {
        self.queue.awaiting_overwrite()
    }

    /// Whether any task is still able to change, which is when polling is worth a timer.
    #[must_use]
    pub fn any_active(&self) -> bool {
        !self.queue.active().is_empty()
    }

    /// Whether anything at all is in the list, which is when the page has content.
    #[cfg(test)]
    #[must_use]
    pub fn any(&self) -> bool {
        !self.queue.transfers().is_empty()
    }

    /// Starts an upload of one local file into a remote directory.
    ///
    /// The remote name is the local file's own name, exactly as it is: a file called
    /// `报告 终稿.txt` arrives under that name, and the directory string is used as the
    /// user typed it.
    ///
    /// # Errors
    ///
    /// Returns [`TransferProblem`] when there is no file name, no directory or no
    /// connection; in each case no task exists afterwards.
    pub fn upload(
        &mut self,
        session: SessionId,
        local: &str,
        directory: Option<&str>,
    ) -> Result<TransferId, TransferProblem> {
        let name = file_name(local).ok_or(TransferProblem::SourceNamesNoFile)?;
        let directory = directory.ok_or(TransferProblem::DirectoryUnknown)?;
        let target = join_path(directory, name);
        self.start(session, TransferDirection::Upload, local.to_owned(), target)
    }

    /// Starts a download of one remote file into a local directory.
    ///
    /// # Errors
    ///
    /// Returns [`TransferProblem`] when either path names no file, when no local
    /// directory was given, or when the connection cannot take the command.
    pub fn download(
        &mut self,
        session: SessionId,
        remote: &str,
        local_directory: &str,
    ) -> Result<TransferId, TransferProblem> {
        let name = file_name(remote).ok_or(TransferProblem::SourceNamesNoFile)?;
        if local_directory.trim().is_empty() {
            return Err(TransferProblem::DirectoryUnknown);
        }
        let target = local_path(Path::new(local_directory.trim()), name)
            .to_string_lossy()
            .into_owned();
        self.start(
            session,
            TransferDirection::Download,
            remote.to_owned(),
            target,
        )
    }

    /// Creates the task and hands it to the session's connection.
    fn start(
        &mut self,
        session: SessionId,
        direction: TransferDirection,
        source: String,
        destination: String,
    ) -> Result<TransferId, TransferProblem> {
        if !self.can_transfer(session) {
            return Err(TransferProblem::CommandRefused);
        }
        let transfer = Transfer::queued(session, direction, source, destination);
        let id = self.queue.add(transfer.clone());
        if self.dispatch(
            session,
            TransferCommand::Start(TransferRequest::for_transfer(&transfer)),
        ) {
            Ok(id)
        } else {
            // A task whose command never left this process must not sit in the list as
            // if a server had been asked: it is reported as what it is.
            self.queue
                .fail_session(session, TransferFailure::ConnectionLost);
            Err(TransferProblem::CommandRefused)
        }
    }

    /// Answers the question about an existing target.
    ///
    /// Returns whether the answer was delivered. A task that already finished ignores it,
    /// which is why a stale button cannot change a report.
    pub fn decide(&mut self, id: TransferId, choice: OverwriteChoice) -> bool {
        let Some(task) = self.queue.get(id) else {
            return false;
        };
        if !task.state().is_awaiting_overwrite() {
            return false;
        }
        let session = task.session();
        self.dispatch(
            session,
            TransferCommand::Decide {
                transfer: id,
                choice,
            },
        )
    }

    /// Stops one task, leaving its target untouched.
    ///
    /// The state is cancelled here so the list cannot keep showing a running transfer
    /// whose worker was told to stop. The worker's own cancellation event then reports a
    /// partial file, when it found one it could not remove.
    pub fn cancel(&mut self, id: TransferId) -> bool {
        let Some(task) = self.queue.get(id) else {
            return false;
        };
        let session = task.session();
        if !self.queue.cancel(id) {
            return false;
        }
        self.dispatch(session, TransferCommand::Cancel { transfer: id });
        true
    }

    /// Starts a new task for the same two paths.
    ///
    /// When the failed task has a checkpoint, the new task carries it and stays queued:
    /// the worker re-checks the partial file before any byte is written, and the old
    /// offset is not progress. Without a checkpoint this is a transfer from the first
    /// byte. The task that failed keeps its own report.
    pub fn retry(&mut self, id: TransferId) -> Option<TransferId> {
        self.launch(self.queue.retry(id)?)
    }

    /// Starts a new task from the first byte, dropping any checkpoint.
    ///
    /// This is the confirmed re-transfer. The failed task stays in the list.
    pub fn restart(&mut self, id: TransferId) -> Option<TransferId> {
        self.launch(self.queue.restart(id)?)
    }

    fn launch(&mut self, transfer: Transfer) -> Option<TransferId> {
        let session = transfer.session();
        let id = self.queue.add(transfer.clone());
        if !self.dispatch(
            session,
            TransferCommand::Start(TransferRequest::for_transfer(&transfer)),
        ) {
            self.queue
                .fail_session(session, TransferFailure::ConnectionLost);
            return None;
        }
        Some(id)
    }

    /// Applies every event that arrived, returning whether anything changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        for channel in self.channels.values_mut() {
            for event in channel.drain() {
                // The queue answers which task the event belongs to; an event nobody
                // owns is dropped here and changes nothing.
                if self.queue.apply(&event).is_some() {
                    changed = true;
                }
            }
        }
        changed
    }

    /// Reports every active task of a session as failed, which is what a connection that
    /// ended leaves behind.
    ///
    /// The reason is the transport's, so the list says a session was lost instead of
    /// showing a transfer that appears to be still running. A task that was moving bytes
    /// keeps its own report: its worker died with the connection and can no longer say
    /// anything, so the queue names the temporary file that task was writing - the file
    /// is still on the far side, and a user who is not told about it finds it later.
    pub fn session_lost(&mut self, session: SessionId, failure: TransferFailure) -> bool {
        !self.queue.fail_session(session, failure).is_empty()
    }

    /// Ends the transfers of a session that is closing.
    ///
    /// Every active task of that session is cancelled; the tasks of every other session
    /// keep their state, because closing one session says nothing about another. What
    /// already happened stays in the list - a failure the user has not read yet is not
    /// improved by the session that produced it disappearing.
    ///
    /// Returns how many tasks were stopped.
    pub fn detach(&mut self, session: SessionId) -> usize {
        let cancelled = self.queue.cancel_session(session);
        for id in &cancelled {
            self.dispatch(session, TransferCommand::Cancel { transfer: *id });
        }
        self.channels.remove(&session);
        cancelled.len()
    }

    /// Sends one command to a session's connection.
    fn dispatch(&mut self, session: SessionId, command: TransferCommand) -> bool {
        self.channels
            .get_mut(&session)
            .is_some_and(|channel| channel.send(command))
    }
}

/// The last name of a path that names a file, written the way the two platforms do.
///
/// Both separators are accepted on any platform: a path copied from a Windows window
/// keeps its meaning when it is used here, and a POSIX path is not mistaken for a single
/// odd name.
#[must_use]
pub fn file_name(path: &str) -> Option<&str> {
    // A trailing separator names a directory, not a file inside it: "C:/work/" is the
    // folder, and copying "work" out of it would write a file the user did not choose.
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.ends_with(['/', '\\']) {
        return None;
    }
    let trimmed = trimmed.trim_end_matches(['/', '\\']);
    let name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed).trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name)
}

/// One name inside a local directory, as a path of this platform.
#[must_use]
pub fn local_path(directory: &Path, name: &str) -> PathBuf {
    directory.join(name)
}

/// Where a download starts when the user has not said otherwise: this account's home.
///
/// It is a starting point, not a decision: the field is editable, and a folder that does
/// not exist is reported by the task as a destination that could not be written.
#[must_use]
pub fn default_download_directory() -> String {
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").ok()
    } else {
        std::env::var("HOME").ok()
    };
    home.filter(|home| !home.trim().is_empty())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shellune_core::ResumeHint;
    use std::sync::{Arc, Mutex};

    /// What a scripted connection holds.
    #[derive(Default)]
    struct Script {
        sent: Vec<TransferCommand>,
        events: Vec<TransferEvent>,
        accepting: bool,
    }

    /// A channel that records commands and answers the ones a test chooses.
    #[derive(Clone, Default)]
    struct FakeChannel(Arc<Mutex<Script>>);

    impl FakeChannel {
        fn accepting() -> Self {
            let channel = Self::default();
            channel.0.lock().expect("a free lock").accepting = true;
            channel
        }

        fn closed() -> Self {
            Self::default()
        }

        fn commands(&self) -> Vec<TransferCommand> {
            self.0.lock().expect("a free lock").sent.clone()
        }

        fn push(&self, event: TransferEvent) {
            self.0.lock().expect("a free lock").events.push(event);
        }

        fn starts(&self) -> Vec<TransferRequest> {
            self.commands()
                .into_iter()
                .filter_map(|command| match command {
                    TransferCommand::Start(request) => Some(request),
                    _ => None,
                })
                .collect()
        }
    }

    impl TransferChannel for FakeChannel {
        fn send(&mut self, command: TransferCommand) -> bool {
            let mut script = self.0.lock().expect("a free lock");
            if !script.accepting {
                return false;
            }
            script.sent.push(command);
            true
        }

        fn drain(&mut self) -> Vec<TransferEvent> {
            std::mem::take(&mut self.0.lock().expect("a free lock").events)
        }
    }

    fn views(session: SessionId, channel: FakeChannel) -> TransferViews {
        let mut views = TransferViews::new();
        views.attach(session, Box::new(channel));
        views
    }

    #[test]
    fn an_upload_names_the_local_file_and_the_directory_that_is_open() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());

        let id = views
            .upload(
                session,
                "C:/work/报告 终稿.txt",
                Some("/home/deploy/文档 与 空格"),
            )
            .expect("the upload starts");
        let starts = channel.starts();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].transfer, id);
        assert_eq!(starts[0].direction, TransferDirection::Upload);
        assert_eq!(starts[0].source, "C:/work/报告 终稿.txt");
        assert_eq!(
            starts[0].destination, "/home/deploy/文档 与 空格/报告 终稿.txt",
            "the remote name is the local name, unchanged"
        );
        assert_eq!(views.active_count(session), 1);
    }

    #[test]
    fn a_path_without_a_name_is_refused_before_a_task_exists() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());

        assert_eq!(
            views.upload(session, "C:/work/", None),
            Err(TransferProblem::SourceNamesNoFile)
        );
        assert_eq!(
            views.upload(session, "C:/work/report.txt", None),
            Err(TransferProblem::DirectoryUnknown)
        );
        assert_eq!(
            views.download(session, "/home/deploy/a.txt", "  "),
            Err(TransferProblem::DirectoryUnknown)
        );
        assert!(
            channel.commands().is_empty(),
            "nothing is sent for a path the window refuses on its own"
        );
        assert!(!views.any());
    }

    #[test]
    fn a_connection_that_cannot_take_the_command_leaves_no_running_task() {
        let session = SessionId::new();
        let mut views = views(session, FakeChannel::closed());
        assert_eq!(
            views.upload(session, "C:/work/a.txt", Some("/home/deploy")),
            Err(TransferProblem::CommandRefused)
        );
        assert_eq!(views.active_count(session), 0, "no task is left behind");
    }

    #[test]
    fn a_target_that_exists_waits_for_the_choice_and_a_keep_ends_the_task() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/report.txt", Some("/home/deploy"))
            .expect("the upload starts");

        channel.push(TransferEvent::target_exists(id, Some(2048)));
        assert!(views.pump());
        let task = views.get(id).expect("the task is listed");
        assert!(task.state().is_awaiting_overwrite());
        assert!(!task.state().is_success());
        assert_eq!(views.awaiting_overwrite().len(), 1);

        assert!(views.decide(id, OverwriteChoice::KeepExisting));
        assert_eq!(
            channel.commands().last(),
            Some(&TransferCommand::Decide {
                transfer: id,
                choice: OverwriteChoice::KeepExisting,
            })
        );
        // The worker answers a "keep" with a cancellation: nothing was written.
        channel.push(TransferEvent {
            transfer: id,
            update: shellune_core::TransferUpdate::Cancelled {
                residue: None,
                resume: None,
            },
        });
        views.pump();
        assert!(
            views.get(id).is_some_and(|task| !task.state().is_success()),
            "keeping the existing file never looks like a finished transfer"
        );
    }

    #[test]
    fn cancelling_a_task_stops_it_without_waiting_for_the_server() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/a.txt", Some("/home/deploy"))
            .expect("the upload starts");

        assert!(views.cancel(id));
        assert_eq!(
            channel.commands().last(),
            Some(&TransferCommand::Cancel { transfer: id })
        );
        assert_eq!(views.active_count(session), 0);
        assert!(
            !views.get(id).is_some_and(|task| task.state().is_success()),
            "a cancelled task is never a success"
        );
        assert!(!views.cancel(id), "a finished task is not cancelled twice");
    }

    #[test]
    fn a_late_event_from_a_forgotten_task_changes_nothing() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/a.txt", Some("/home/deploy"))
            .expect("the upload starts");

        // A retry is a new task; the old one is finished and its events are stale.
        views.cancel(id);
        let retried = views.retry(id).expect("a cancelled task can be retried");
        assert_ne!(retried, id, "a retry is a new task");

        channel.push(TransferEvent {
            transfer: id,
            update: shellune_core::TransferUpdate::Completed { bytes: 10 },
        });
        assert!(
            !views.pump(),
            "an event belongs to the cancelled task, not to the new one"
        );
        assert!(
            views
                .get(retried)
                .is_some_and(|task| task.state() == &shellune_core::TransferState::Queued),
            "the new task is still queued"
        );
    }

    #[test]
    fn a_retry_starts_from_zero_bytes_under_a_new_identifier() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/报告.txt", Some("/home/deploy"))
            .expect("the upload starts");
        channel.push(TransferEvent::progress(id, 4096, Some(8192)));
        channel.push(TransferEvent {
            transfer: id,
            update: shellune_core::TransferUpdate::Failed {
                failure: TransferFailure::ConnectionLost,
                residue: None,
                resume: None,
            },
        });
        views.pump();
        assert_eq!(views.active_count(session), 0);

        let retried = views.retry(id).expect("a failed task can be retried");
        let starts = channel.starts();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[1].transfer, retried);
        assert_eq!(starts[1].source, "C:/work/报告.txt");
        assert_eq!(
            views.get(retried).expect("the new task").state(),
            &shellune_core::TransferState::Queued,
            "a retry starts at the beginning, never in the middle"
        );
        assert!(channel.starts()[1].resume.is_none());
    }

    #[test]
    fn a_checkpoint_is_retried_without_pretending_the_old_offset_and_a_restart_drops_it() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/a.txt", Some("/home/deploy"))
            .expect("the upload starts");
        let hint = ResumeHint {
            partial: "/home/deploy/.shellune-part-kept".to_owned(),
            record: "C:/temp/kept.json".to_owned(),
            offset: 2048,
            generation: 1,
        };
        channel.push(TransferEvent::progress(id, 2048, Some(4096)));
        channel.push(TransferEvent {
            transfer: id,
            update: shellune_core::TransferUpdate::Failed {
                failure: TransferFailure::ConnectionLost,
                residue: Some(hint.partial.clone()),
                resume: Some(hint.clone()),
            },
        });
        views.pump();

        let retried = views.retry(id).expect("the failure can be retried");
        let resumed = &channel.starts()[1];
        assert_eq!(resumed.transfer, retried);
        assert_eq!(resumed.resume.as_ref(), Some(&hint));
        assert_eq!(
            views.get(retried).expect("the new task").state(),
            &shellune_core::TransferState::Queued,
            "the confirmed offset is not shown as progress before the worker re-checks it"
        );
        assert!(
            views.get(id).is_some(),
            "the failed task stays beside the new one"
        );

        let fresh = views.restart(id).expect("the failure can start over");
        assert!(channel.starts()[2].resume.is_none());
        assert_ne!(fresh, id);
        assert_ne!(fresh, retried);
    }

    #[test]
    fn closing_one_session_cancels_only_its_own_transfers() {
        let first = SessionId::new();
        let second = SessionId::new();
        let first_channel = FakeChannel::accepting();
        let second_channel = FakeChannel::accepting();
        let mut views = TransferViews::new();
        views.attach(first, Box::new(first_channel.clone()));
        views.attach(second, Box::new(second_channel.clone()));
        let first_upload = views
            .upload(first, "C:/work/a.txt", Some("/home/deploy"))
            .expect("the first upload starts");
        let second_upload = views
            .upload(second, "C:/work/b.txt", Some("/home/deploy"))
            .expect("the second upload starts");

        views.detach(first);
        assert!(!views.can_transfer(first));
        assert_eq!(views.active_count(first), 0);
        assert_eq!(
            views.active_count(second),
            1,
            "the other session's transfer keeps running"
        );
        assert_eq!(
            first_channel.commands().last(),
            Some(&TransferCommand::Cancel {
                transfer: first_upload
            })
        );
        assert!(
            !second_channel
                .commands()
                .iter()
                .any(|command| matches!(command, TransferCommand::Cancel { .. })),
            "no cancellation reaches the session that stays open"
        );
        assert_eq!(
            views.get(second_upload).map(Transfer::id),
            Some(second_upload)
        );
    }

    #[test]
    fn a_lost_connection_fails_the_transfers_it_was_running() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/a.txt", Some("/home/deploy"))
            .expect("the upload starts");

        assert!(views.session_lost(session, TransferFailure::ConnectionLost));
        let task = views.get(id).expect("the task is listed");
        assert_eq!(
            task.state().failure(),
            Some(&TransferFailure::ConnectionLost)
        );
        assert!(!task.state().is_success());
        assert_eq!(
            task.state().residue(),
            None,
            "a task that never started wrote nothing, so nothing is named"
        );
        assert!(!views.any_active());
    }

    #[test]
    fn a_lost_connection_names_the_partial_file_the_task_was_writing() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut uploads = views(session, channel.clone());
        let id = uploads
            .upload(session, "C:/work/report.txt", Some("/home/deploy"))
            .expect("the upload starts");
        channel.push(TransferEvent::progress(id, 2048, Some(4096)));
        uploads.pump();
        let expected = format!("/home/deploy/{}{id}", shellune_core::PARTIAL_PREFIX);
        assert_eq!(
            uploads.get(id).expect("the task is listed").partial_path(),
            expected,
            "the residue is the temporary file the worker writes, in the target directory"
        );

        assert!(uploads.session_lost(session, TransferFailure::ConnectionLost));
        let task = uploads.get(id).expect("the task is listed");
        assert_eq!(
            task.state().failure(),
            Some(&TransferFailure::ConnectionLost),
            "the reason is still the transport's"
        );
        assert_eq!(
            task.state().residue(),
            Some(expected.as_str()),
            "the file the bytes were going into is named instead of being dropped with the session"
        );

        // A download names the same way, beside its local target.
        let second = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut downloads = views(second, channel.clone());
        let id = downloads
            .download(second, "/home/deploy/说明.txt", "C:/work")
            .expect("the download starts");
        channel.push(TransferEvent::progress(id, 128, Some(256)));
        downloads.pump();
        let expected = downloads
            .get(id)
            .expect("the task is listed")
            .partial_path()
            .to_owned();
        assert!(downloads.session_lost(second, TransferFailure::ConnectionLost));
        assert_eq!(
            downloads
                .get(id)
                .expect("the task is listed")
                .state()
                .residue(),
            Some(expected.as_str()),
            "a download names the local temporary file it was writing"
        );
    }

    #[test]
    fn a_progress_report_moves_the_number_and_never_a_finished_one() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .download(session, "/home/deploy/说明.txt", "C:/work")
            .expect("the download starts");
        channel.push(TransferEvent::progress(id, 512, Some(1024)));
        assert!(views.pump());
        let task = views.get(id).expect("the task is listed");
        assert_eq!(task.state().transferred(), Some(512));
        assert_eq!(task.state().permille(), Some(500));
        assert_eq!(
            task.destination(),
            local_path(Path::new("C:/work"), "说明.txt")
                .to_string_lossy()
                .as_ref()
        );
    }

    #[test]
    fn both_separators_are_understood_by_the_name_of_a_path() {
        assert_eq!(file_name("C:/work/report.txt"), Some("report.txt"));
        assert_eq!(file_name("C:\\work\\report.txt"), Some("report.txt"));
        assert_eq!(
            file_name("/home/deploy/文档 终稿.txt"),
            Some("文档 终稿.txt")
        );
        assert_eq!(file_name("C:/work/"), None);
        assert_eq!(file_name("/"), None);
        assert_eq!(file_name(""), None);
        assert_eq!(file_name("/home/deploy/."), None);
    }
}
