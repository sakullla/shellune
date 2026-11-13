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
use std::time::{Duration, Instant};

use shellune_core::{
    OverwriteChoice, SessionId, Transfer, TransferDirection, TransferEvent, TransferFailure,
    TransferId, TransferProtocol, TransferQueue, TransferUpdate, join_path,
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
    /// Whether a later settlement can still arrive.
    ///
    /// A scripted channel stays open: the test decides when the worker's report is
    /// pushed. A live connection reports false once the worker's sender is gone.
    fn events_open(&self) -> bool {
        true
    }
}

impl TransferChannel for TransferLink {
    fn send(&mut self, command: TransferCommand) -> bool {
        TransferLink::send(self, command)
    }

    fn drain(&mut self) -> Vec<TransferEvent> {
        TransferLink::drain(self)
    }

    fn events_open(&self) -> bool {
        TransferLink::events_open(self)
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
    /// The source is a directory and copying it as a whole was not chosen.
    ///
    /// A directory is never expanded into a tree behind the user's back: the choice is
    /// explicit, and a task without it would copy nothing meaningful.
    DirectoryNeedsRecursion,
    /// The source is a directory and the chosen protocol copies one file at a time.
    ///
    /// SFTP copies a file; a directory source needs the other protocol and the
    /// recursive choice, so the reason names that next step instead of saying the path
    /// has no name - which is what a directory would otherwise be called.
    DirectoryNeedsScp,
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
    /// The path names a directory and copying it whole was not chosen.
    NeedsRecursion,
    /// The path names a directory and only the other protocol copies directories.
    DirectoryNeedsScp,
}

/// How fast one task moved, measured from the bytes it reported.
///
/// The queue knows the bytes; only the window knows when it read them. Keeping the two
/// apart is what lets the list say "1.2 MB/s" and "0:12 left" without the core inventing
/// a clock: a speed exists only after two real samples of a running task, and every
/// sample is a number the worker sent.
#[derive(Clone, Copy, Debug)]
struct Pace {
    /// When the last sample was taken.
    at: Instant,
    /// The bytes reported at that sample.
    bytes: u64,
    /// Bytes per second, smoothed across samples. Zero until two samples exist.
    rate: f64,
}

/// How long two samples have to be apart before they are a measurement.
///
/// A shorter gap turns the rounding of two neighbouring progress reports into a wild
/// speed; a longer one makes the number lag behind the bytes on screen.
const PACE_SAMPLE: Duration = Duration::from_millis(400);

/// The speed a task has to be moving at before an estimate is worth printing.
///
/// A rate of a few bytes per second is a stall, not a speed: dividing the remaining
/// bytes by it would promise hours, which is a number no one can act on.
const PACE_FLOOR: f64 = 1024.0;

/// The transfers of every session this window can move files on.
#[derive(Default)]
pub struct TransferViews {
    channels: HashMap<SessionId, Box<dyn TransferChannel>>,
    /// Connections that have ended but whose workers may still report a settlement.
    settling: HashMap<SessionId, Box<dyn TransferChannel>>,
    /// Tasks whose worker has not yet reported the flush it was in the middle of.
    pending_settlement: HashMap<SessionId, Vec<TransferId>>,
    /// The speed each running task was last seen moving at.
    paces: HashMap<TransferId, Pace>,
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

    /// The speed one running task was last measured at, in bytes per second.
    ///
    /// None before there are two samples, and for a task that is not moving bytes: a
    /// queued task has not been measured at all, and a finished one has no speed left.
    #[must_use]
    pub fn rate(&self, id: TransferId) -> Option<f64> {
        let pace = self.paces.get(&id)?;
        (pace.rate >= PACE_FLOOR).then_some(pace.rate)
    }

    /// How long the bytes still missing take at the measured speed.
    ///
    /// None when either half of the estimate is unknown: a task whose size the worker
    /// has not reported has no remainder, and one with no measured speed has no forecast.
    /// Nothing here is a guess from a partial file or from the size of one file in a tree.
    #[must_use]
    pub fn remaining(&self, id: TransferId) -> Option<Duration> {
        let rate = self.rate(id)?;
        let task = self.queue.get(id)?;
        let total = task.state().total()?;
        let done = task.state().transferred()?;
        let left = total.saturating_sub(done) as f64;
        Some(Duration::from_secs_f64((left / rate).max(0.0)))
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

    /// Whether a closed session can still report the prefix it flushed.
    ///
    /// The list already shows the task as stopped. Polling continues only so the
    /// worker's settlement can replace a stale checkpoint with the offset it actually
    /// wrote.
    #[must_use]
    pub fn awaiting_settlement(&self) -> bool {
        !self.settling.is_empty()
    }

    /// Whether anything at all is in the list, which is when the page has content.
    #[cfg(test)]
    #[must_use]
    pub fn any(&self) -> bool {
        !self.queue.transfers().is_empty()
    }

    /// Starts an upload of one local file into a remote directory over SFTP.
    ///
    /// The remote name is the local file's own name, exactly as it is: a file called
    /// `报告 终稿.txt` arrives under that name, and the directory string is used as the
    /// user typed it. The window always picks a protocol explicitly
    /// ([`Self::upload_over`]); this is the shorthand the module's own tests use.
    ///
    /// # Errors
    ///
    /// Returns [`TransferProblem`] when there is no file name, no directory or no
    /// connection; in each case no task exists afterwards.
    #[cfg(test)]
    pub fn upload(
        &mut self,
        session: SessionId,
        local: &str,
        directory: Option<&str>,
    ) -> Result<TransferId, TransferProblem> {
        self.upload_over(session, local, directory, TransferProtocol::Sftp, false)
    }

    /// Starts an upload over one chosen protocol, and as a tree when that was chosen.
    ///
    /// The protocol is the task's own: it never falls back to the other one, and a
    /// directory is copied only when `recursive` says the user asked for it.
    ///
    /// # Errors
    ///
    /// Returns [`TransferProblem`] when there is no file name, no directory, no
    /// connection, or when the source is a directory that would not be copied whole.
    pub fn upload_over(
        &mut self,
        session: SessionId,
        local: &str,
        directory: Option<&str>,
        protocol: TransferProtocol,
        recursive: bool,
    ) -> Result<TransferId, TransferProblem> {
        let name = file_name(local).ok_or(TransferProblem::SourceNamesNoFile)?;
        let directory = directory.ok_or(TransferProblem::DirectoryUnknown)?;
        if !protocol.copies_directories() && is_directory(Path::new(local.trim())) {
            // SFTP copies a file, so a directory source would be refused by the worker
            // anyway. The window says which protocol can copy it rather than calling a
            // directory "a path without a name", which would name the wrong problem.
            return Err(TransferProblem::DirectoryNeedsScp);
        }
        if !recursive && is_directory(Path::new(local.trim())) {
            return Err(TransferProblem::DirectoryNeedsRecursion);
        }
        let target = join_path(directory, name);
        let task = Transfer::queued_over(
            session,
            protocol,
            TransferDirection::Upload,
            local.to_owned(),
            target,
        )
        .with_recursive(recursive);
        self.start(session, task)
    }

    /// Starts a download of one remote file into a local directory over SFTP.
    ///
    /// [`Self::download_over`] is the same task with a protocol the caller chose.
    ///
    /// # Errors
    ///
    /// Returns [`TransferProblem`] when either path names no file, when no local
    /// directory was given, or when the connection cannot take the command.
    #[cfg(test)]
    pub fn download(
        &mut self,
        session: SessionId,
        remote: &str,
        local_directory: &str,
    ) -> Result<TransferId, TransferProblem> {
        self.download_over(
            session,
            remote,
            local_directory,
            TransferProtocol::Sftp,
            false,
        )
    }

    /// Starts a download over one chosen protocol, and of a whole tree when chosen.
    ///
    /// Unlike an upload, whether the remote path is a directory is not known here: the
    /// worker asks the server, and refuses a directory when recursion was not chosen.
    ///
    /// # Errors
    ///
    /// Returns [`TransferProblem`] for the same reasons [`Self::download`] does.
    pub fn download_over(
        &mut self,
        session: SessionId,
        remote: &str,
        local_directory: &str,
        protocol: TransferProtocol,
        recursive: bool,
    ) -> Result<TransferId, TransferProblem> {
        let name = file_name(remote).ok_or(TransferProblem::SourceNamesNoFile)?;
        if local_directory.trim().is_empty() {
            return Err(TransferProblem::DirectoryUnknown);
        }
        let target = local_path(Path::new(local_directory.trim()), name)
            .to_string_lossy()
            .into_owned();
        let task = Transfer::queued_over(
            session,
            protocol,
            TransferDirection::Download,
            remote.to_owned(),
            target,
        )
        .with_recursive(recursive);
        self.start(session, task)
    }

    /// Creates the task and hands it to the session's connection.
    fn start(
        &mut self,
        session: SessionId,
        transfer: Transfer,
    ) -> Result<TransferId, TransferProblem> {
        if !self.can_transfer(session) {
            return Err(TransferProblem::CommandRefused);
        }
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
        let mut live = Vec::new();
        for channel in self.channels.values_mut() {
            live.extend(channel.drain());
        }
        let sessions: Vec<SessionId> = self.settling.keys().copied().collect();
        let mut settled = Vec::new();
        let mut disconnected = Vec::new();
        for session in sessions {
            let Some(channel) = self.settling.get_mut(&session) else {
                continue;
            };
            let events = channel.drain();
            let open = channel.events_open();
            settled.push((session, events));
            if !open {
                disconnected.push(session);
            }
        }
        for event in live {
            if self.queue.apply(&event).is_some() {
                changed = true;
            }
        }
        for (session, events) in settled {
            for event in events {
                // The worker is done with this task even when the settlement only
                // confirms the checkpoint the window already had.
                self.observe_settlement(session, &event);
                if self.queue.apply(&event).is_some() {
                    changed = true;
                }
            }
        }
        for session in disconnected {
            self.settling.remove(&session);
            self.pending_settlement.remove(&session);
        }
        let finished: Vec<SessionId> = self
            .pending_settlement
            .iter()
            .filter(|(_, ids)| ids.is_empty())
            .map(|(session, _)| *session)
            .collect();
        for session in finished {
            self.pending_settlement.remove(&session);
            self.settling.remove(&session);
        }
        self.sample(Instant::now());
        changed
    }

    /// Measures the speed of every running task from the bytes it has reported.
    ///
    /// Reading the clock here, on the frame that applied the bytes, is what makes the
    /// number a measurement: two samples of the same task are divided by the time between
    /// them. A task that reported no new bytes in the gap measures zero, so a stalled
    /// transfer loses its speed instead of keeping the last one it had - and with it the
    /// estimate built on it.
    fn sample(&mut self, now: Instant) {
        let running: Vec<(TransferId, u64)> = self
            .queue
            .transfers()
            .iter()
            .filter_map(|task| match task.state() {
                shellune_core::TransferState::Running { transferred, .. } => {
                    Some((task.id(), *transferred))
                }
                _ => None,
            })
            .collect();
        self.paces
            .retain(|id, _| running.iter().any(|(other, _)| other == id));
        for (id, bytes) in running {
            match self.paces.get_mut(&id) {
                Some(pace) => {
                    let elapsed = now.saturating_duration_since(pace.at);
                    if elapsed < PACE_SAMPLE {
                        continue;
                    }
                    let seconds = elapsed.as_secs_f64();
                    let moved = bytes.saturating_sub(pace.bytes) as f64;
                    // No byte in the whole gap is a stall, not a slow transfer: keeping
                    // the last speed would keep an estimate that is no longer true.
                    pace.rate = if moved == 0.0 {
                        0.0
                    } else if pace.rate > 0.0 {
                        // The first measured rate is taken as it is; later ones are
                        // averaged, so one fast or slow report does not become the speed.
                        pace.rate * 0.5 + (moved / seconds) * 0.5
                    } else {
                        moved / seconds
                    };
                    pace.at = now;
                    pace.bytes = bytes;
                }
                None => {
                    self.paces.insert(
                        id,
                        Pace {
                            at: now,
                            bytes,
                            rate: 0.0,
                        },
                    );
                }
            }
        }
    }

    /// Puts one task in the list in a state a capture can photograph.
    ///
    /// No connection is asked and no byte is written: the task, its progress and its
    /// failure are applied to the queue exactly as a worker's events would be, so the row
    /// a shot shows is the row this build draws from real state (R2).
    #[cfg(feature = "ui-capture")]
    pub fn seed_for_capture(
        &mut self,
        task: Transfer,
        moved: u64,
        total: Option<u64>,
        failed: Option<TransferFailure>,
        residue: Option<String>,
        resume: Option<shellune_core::ResumeHint>,
    ) -> TransferId {
        let id = self.queue.add(task);
        let _ = self.queue.apply(&TransferEvent::started(id));
        let _ = self.queue.apply(&TransferEvent::progress(id, moved, total));
        if let Some(failure) = failed {
            let _ = self.queue.apply(&TransferEvent {
                transfer: id,
                update: TransferUpdate::Failed {
                    failure,
                    residue,
                    resume,
                },
            });
        }
        id
    }

    /// Puts one task in the list with two real progress reports, so a capture shows the
    /// speed and the time left the window measures rather than a made-up number.
    ///
    /// The two reports are divided by the same sampler the window uses, at the interval
    /// the measurement needs; the row a shot draws is therefore the row this build would
    /// draw from a worker that kept reporting (R2, R6).
    #[cfg(feature = "ui-capture")]
    pub fn seed_measured_for_capture(
        &mut self,
        task: Transfer,
        first: u64,
        second: u64,
        total: Option<u64>,
    ) -> TransferId {
        let id = self.queue.add(task);
        let _ = self.queue.apply(&TransferEvent::started(id));
        let _ = self.queue.apply(&TransferEvent::progress(id, first, total));
        let started = Instant::now();
        self.sample(started);
        let _ = self
            .queue
            .apply(&TransferEvent::progress(id, second, total));
        self.sample(started + Duration::from_millis(500));
        id
    }

    /// Measures every running task at a stated moment, for a test.
    ///
    /// The window samples on its own frame timer; a test cannot wait for one, so it
    /// states the moments instead. Nothing else about the measurement changes.
    #[cfg(test)]
    pub fn sample_at_for_test(&mut self, now: Instant) {
        self.sample(now);
    }

    /// Forgets a task whose worker has reported that it stopped.
    fn observe_settlement(&mut self, session: SessionId, event: &TransferEvent) {
        let done = matches!(
            event.update,
            TransferUpdate::Failed { .. }
                | TransferUpdate::Cancelled { .. }
                | TransferUpdate::Completed { .. }
        );
        if !done {
            return;
        }
        if let Some(waiting) = self.pending_settlement.get_mut(&session) {
            waiting.retain(|id| *id != event.transfer);
        }
    }

    /// Keeps the connection until each of `ids` has reported its settlement.
    ///
    /// Dropping the link first discards that report. The window would keep the last
    /// checkpoint it drew, while the worker writes a later offset into the same record.
    fn note_settlement(&mut self, session: SessionId, ids: Vec<TransferId>) {
        if ids.is_empty() {
            if self
                .pending_settlement
                .get(&session)
                .is_none_or(|waiting| waiting.is_empty())
            {
                self.channels.remove(&session);
                self.settling.remove(&session);
                self.pending_settlement.remove(&session);
            }
            return;
        }
        self.pending_settlement
            .entry(session)
            .or_default()
            .extend(ids);
        if let Some(channel) = self.channels.remove(&session) {
            self.settling.insert(session, channel);
        }
    }

    /// Reports every active task of a session as failed, which is what a connection that
    /// ended leaves behind.
    ///
    /// The reason is the transport's, so the list says a session was lost instead of
    /// showing a transfer that appears to be still running. The partial file named at
    /// that moment is the one the task was writing. The worker may still settle a later
    /// flushed prefix; that report replaces the checkpoint, and it is not dropped with
    /// the link.
    pub fn session_lost(&mut self, session: SessionId, failure: TransferFailure) -> bool {
        let failed = self.queue.fail_session(session, failure);
        // The worker can still be settling the prefix it flushed after the last
        // checkpoint. The link stays until that report arrives.
        self.note_settlement(session, failed.clone());
        self.pump();
        !failed.is_empty()
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
        self.note_settlement(session, cancelled.clone());
        self.pump();
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

/// Whether a local path is a directory.
///
/// It is a question about this computer, so it is answered here and not by the worker:
/// a directory that would not be copied as a whole is refused before a task exists.
fn is_directory(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
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

    #[test]
    fn a_lost_connection_keeps_a_resumed_partial_and_a_later_settlement() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/report.txt", Some("/home/deploy"))
            .expect("the upload starts");
        let stale = ResumeHint {
            partial: "/home/deploy/.shellune-part-original".to_owned(),
            record: "C:/temp/original.json".to_owned(),
            offset: 1024,
            generation: 1,
        };
        channel.push(TransferEvent::progress(id, 1024, Some(8192)));
        channel.push(TransferEvent {
            transfer: id,
            update: shellune_core::TransferUpdate::Checkpoint(stale.clone()),
        });
        views.pump();
        let invented = views.get(id).expect("the task is listed").partial_path();
        assert_ne!(invented, stale.partial);

        assert!(views.session_lost(session, TransferFailure::ConnectionLost));
        assert!(
            !views.can_transfer(session),
            "a lost connection cannot start another transfer"
        );
        let frozen = views.get(id).expect("the task stays listed");
        assert_eq!(
            frozen.state().failure(),
            Some(&TransferFailure::ConnectionLost)
        );
        assert_eq!(frozen.state().residue(), Some(stale.partial.as_str()));
        assert_eq!(frozen.state().resume().map(|hint| hint.offset), Some(1024));
        assert!(views.awaiting_settlement());

        let flushed = ResumeHint {
            offset: 4096,
            ..stale.clone()
        };
        channel.push(TransferEvent {
            transfer: id,
            update: shellune_core::TransferUpdate::Failed {
                failure: TransferFailure::ConnectionLost,
                residue: Some(flushed.partial.clone()),
                resume: Some(flushed.clone()),
            },
        });
        assert!(views.pump());
        let settled = views.get(id).expect("the task stays listed");
        assert_eq!(settled.state().resume(), Some(&flushed));
        assert_eq!(settled.state().residue(), Some(flushed.partial.as_str()));
        assert!(!settled.state().is_success());
        assert!(
            !views.awaiting_settlement(),
            "the worker's settlement is the last report"
        );
        let retried = views.retry(id);
        assert!(
            retried.is_none(),
            "the lost connection cannot start the resume itself"
        );
    }

    #[test]
    fn closing_a_session_accepts_the_workers_later_checkpoint_only_for_that_session() {
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
        let stale = ResumeHint {
            partial: "/home/deploy/.shellune-part-original".to_owned(),
            record: "C:/temp/original.json".to_owned(),
            offset: 2048,
            generation: 1,
        };
        first_channel.push(TransferEvent::progress(first_upload, 2048, Some(8192)));
        first_channel.push(TransferEvent {
            transfer: first_upload,
            update: shellune_core::TransferUpdate::Checkpoint(stale.clone()),
        });
        second_channel.push(TransferEvent::progress(second_upload, 32, Some(64)));
        views.pump();

        views.detach(first);
        assert!(!views.can_transfer(first));
        assert_eq!(views.active_count(first), 0);
        assert_eq!(views.active_count(second), 1);
        assert_eq!(
            views
                .get(first_upload)
                .and_then(|task| task.state().residue().map(str::to_owned)),
            Some(stale.partial.clone())
        );

        let flushed = ResumeHint {
            offset: 6144,
            ..stale
        };
        first_channel.push(TransferEvent {
            transfer: first_upload,
            update: shellune_core::TransferUpdate::Cancelled {
                residue: Some(flushed.partial.clone()),
                resume: Some(flushed.clone()),
            },
        });
        // A late completion of the closed session must not be applied as success, and
        // the open session's channel is a different one.
        second_channel.push(TransferEvent {
            transfer: first_upload,
            update: shellune_core::TransferUpdate::Completed { bytes: 8192 },
        });
        views.pump();
        let closed = views.get(first_upload).expect("the task stays listed");
        assert_eq!(closed.state().resume(), Some(&flushed));
        assert!(!closed.state().is_success());
        assert_eq!(
            views
                .get(second_upload)
                .and_then(|task| task.state().transferred()),
            Some(32)
        );
        assert_eq!(views.active_count(second), 1);
    }

    #[test]
    fn a_queued_task_has_no_measured_speed_and_a_running_one_only_after_two_samples() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/report.txt", Some("/home/deploy"))
            .expect("the upload starts");
        assert_eq!(
            views.get(id).map(|task| task.state().clone()),
            Some(shellune_core::TransferState::Queued),
            "a task whose worker has not started is queued"
        );
        assert_eq!(
            views.rate(id),
            None,
            "nothing has moved, so nothing was measured"
        );
        assert_eq!(views.remaining(id), None);

        let first = Instant::now();
        // The worker has started and moved two megabytes in four seconds.
        channel.push(TransferEvent::started(id));
        channel.push(TransferEvent::progress(
            id,
            1024 * 1024,
            Some(8 * 1024 * 1024),
        ));
        views.pump();
        views.sample_at_for_test(first);
        assert_eq!(
            views.rate(id),
            None,
            "one sample is a position, not a speed"
        );

        channel.push(TransferEvent::progress(
            id,
            5 * 1024 * 1024,
            Some(8 * 1024 * 1024),
        ));
        views.pump();
        views.sample_at_for_test(first + Duration::from_secs(4));
        let rate = views.rate(id).expect("two samples are a speed");
        assert!(
            (rate - (4.0 * 1024.0 * 1024.0) / 4.0).abs() < 64.0 * 1024.0,
            "the speed is the bytes between the samples over the time between them: {rate}"
        );
        let left = views.remaining(id).expect("three megabytes are left");
        assert!(
            left.as_secs_f64() > 2.0 && left.as_secs_f64() < 4.0,
            "the remainder is the missing bytes at that speed: {left:?}"
        );

        // A stalled transfer stops claiming a speed: the same bytes over the next gap.
        views.sample_at_for_test(first + Duration::from_secs(12));
        assert!(
            views.rate(id).is_none(),
            "a task that reported no bytes has no measured speed"
        );
        assert_eq!(views.remaining(id), None);
    }

    #[test]
    fn a_task_whose_size_is_unknown_reports_bytes_but_no_remaining_time() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        let id = views
            .upload(session, "C:/work/tree", Some("/home/deploy"))
            .expect("the upload starts");
        let start = Instant::now();
        channel.push(TransferEvent::started(id));
        channel.push(TransferEvent::progress(id, 0, None));
        views.pump();
        views.sample_at_for_test(start);
        channel.push(TransferEvent::progress(id, 2048 * 1024, None));
        views.pump();
        views.sample_at_for_test(start + Duration::from_secs(2));
        assert!(views.rate(id).is_some(), "the bytes moved are measured");
        assert_eq!(
            views.remaining(id),
            None,
            "a tree whose size the worker does not know has no remainder to estimate"
        );
    }
}
