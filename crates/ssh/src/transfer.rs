//! Uploading and downloading files over the connection the terminal authenticated (R10, R17).
//!
//! A transfer is its own task on its own SFTP channel, so a long upload never stops the
//! terminal from reading or writing, and a second transfer runs beside the first. Four
//! rules decide what this module may report:
//!
//! * nothing is written into an existing target before the user answers
//!   [`TransferUpdate::TargetExists`]. Until that answer the task is held, so a session
//!   that closes or a transfer that is cancelled leaves the original file exactly as it
//!   was;
//! * the bytes go into a recognizable temporary file (`.shellune-part-*`) next to the
//!   target. Only after that file is closed and its size checked is it renamed onto the
//!   target, which is why [`TransferUpdate::Completed`] means "the target holds the
//!   content" and never "the temporary file was written";
//! * a cancellation or failure removes the temporary file when the bytes already
//!   written cannot be checked again. A prefix that was flushed and recorded is kept,
//!   with its checkpoint, so a later attempt can resume it. A partial file nobody
//!   mentions is still worse than one that is named. The other case that keeps a file
//!   is a replacement that failed after the old target was already gone: there the
//!   completed copy is the only one left, so it is kept and named;
//! * every event carries the task's [`TransferId`], so a message from an attempt that
//!   already ended cannot make a newer task look finished. A new attempt resumes only
//!   when its request carries a checkpoint and the source and partial file still match
//!   it. Otherwise it reports that refusal and writes nothing, and a restart with no
//!   checkpoint begins at the first byte.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{Config, SftpSession};
use russh_sftp::protocol::{OpenFlags, StatusCode};
use sha2::{Digest, Sha256};
use shellune_core::{
    OverwriteChoice, PartialRecord, ResumeHint, ResumeRefusal, Transfer, TransferDirection,
    TransferEvent, TransferFailure, TransferId, TransferUpdate, parent_path,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::session::Command;
use crate::sftp::{ChannelFailure, REQUEST_TIMEOUT_SECS};

/// The prefix of the file a transfer writes before the target's own name exists.
///
/// It is recognizable on purpose: a leftover of this shape names itself as a partial
/// transfer, in a directory listing, without any other tool having to explain it. The
/// value and the rule that builds the path live in shellune-core, so the window can name
/// the same file after a connection that ended.
pub const PARTIAL_PREFIX: &str = shellune_core::PARTIAL_PREFIX;

/// How many bytes are moved between two checks of the cancel channel.
///
/// Small enough that a cancel is noticed within one chunk, large enough that a transfer
/// of a few megabytes is not a million round trips.
const CHUNK: usize = 64 * 1024;

/// How often progress is reported at most.
///
/// The view needs a number that moves, not one update per chunk: an unbounded stream of
/// reports would spend the window's time drawing bytes nobody can read.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

/// One transfer the application asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferRequest {
    /// The task's identifier, carried by every event it produces.
    pub transfer: TransferId,
    /// Which way the bytes move.
    pub direction: TransferDirection,
    /// Local file for an upload, remote path for a download.
    pub source: String,
    /// Remote path for an upload, local file for a download.
    pub destination: String,
    /// A checkpoint to re-check before continuing. `None` starts at the first byte.
    pub resume: Option<ResumeHint>,
}

impl TransferRequest {
    /// The request that performs one queued transfer.
    #[must_use]
    pub fn for_transfer(transfer: &Transfer) -> Self {
        Self {
            transfer: transfer.id(),
            direction: transfer.direction(),
            source: transfer.source().to_owned(),
            destination: transfer.destination().to_owned(),
            resume: transfer.resume_hint().cloned(),
        }
    }
}

/// What the window tells a transfer task to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferCommand {
    /// Start the task described by the request.
    Start(TransferRequest),
    /// Answer the question about an existing target.
    Decide {
        /// The task the answer belongs to.
        transfer: TransferId,
        /// What the user chose.
        choice: OverwriteChoice,
    },
    /// Stop the task, leaving the target untouched.
    Cancel {
        /// The task to stop.
        transfer: TransferId,
    },
}

/// One message from the window to a running task.
enum TaskMessage {
    Decide(OverwriteChoice),
    Cancel,
}

/// The connection thread's side of the transfer tasks.
pub(crate) struct Wiring {
    /// Commands the window sent.
    pub commands: UnboundedReceiver<TransferCommand>,
    /// Where the tasks report to.
    pub events: UnboundedSender<TransferEvent>,
}

/// The application's side of one connection's transfers.
///
/// It works like [`crate::FileLink`]: a closed link answers false instead of queueing a
/// command nobody will serve, so the window can report a connection that is gone rather
/// than wait for an answer that will never come.
pub struct TransferLink {
    commands: UnboundedSender<TransferCommand>,
    events: UnboundedReceiver<TransferEvent>,
    closed: bool,
}

impl TransferLink {
    /// A link over the two channels the session created for it.
    pub(crate) fn new(
        commands: UnboundedSender<TransferCommand>,
        events: UnboundedReceiver<TransferEvent>,
    ) -> Self {
        Self {
            commands,
            events,
            closed: false,
        }
    }

    /// Asks for something, returning whether the request reached the connection.
    pub fn send(&mut self, command: TransferCommand) -> bool {
        if self.closed {
            return false;
        }
        if self.commands.send(command).is_err() {
            self.closed = true;
            return false;
        }
        true
    }

    /// Every event that arrived since the last call.
    pub fn drain(&mut self) -> Vec<TransferEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Stops sending; used when the session is closed.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Whether the link stopped accepting commands.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Serves transfer commands until the connection ends.
///
/// Each task runs on its own, so one slow file neither blocks the next transfer nor the
/// terminal. The map only exists to route a decision or a cancellation to the task that
/// asked for it.
pub(crate) async fn serve(wiring: Wiring, commands: UnboundedSender<Command>) {
    let Wiring {
        commands: mut incoming,
        events,
    } = wiring;
    let mut tasks: HashMap<TransferId, UnboundedSender<TaskMessage>> = HashMap::new();
    while let Some(command) = incoming.recv().await {
        match command {
            TransferCommand::Start(request) => {
                let id = request.transfer;
                let (messages, receiver) = unbounded_channel();
                tasks.insert(id, messages);
                tokio::spawn(run(request, commands.clone(), events.clone(), receiver));
            }
            TransferCommand::Decide { transfer, choice } => {
                if let Some(sender) = tasks.get(&transfer) {
                    let _ = sender.send(TaskMessage::Decide(choice));
                }
            }
            TransferCommand::Cancel { transfer } => {
                if let Some(sender) = tasks.get(&transfer) {
                    let _ = sender.send(TaskMessage::Cancel);
                }
            }
        }
        // A task whose receiver is gone cannot be told anything any more.
        tasks.retain(|_, sender| !sender.is_closed());
    }
}

/// What one task ended with.
enum End {
    /// The target holds the transferred content.
    Completed(u64),
    /// The transfer stopped. The path is a partial file that stayed, and the hint is
    /// present only when that file was kept because its prefix can be checked again.
    Failed(TransferFailure, Option<String>, Option<ResumeHint>),
    /// The user stopped it. The hint is present only for a prefix that was kept.
    Cancelled(Option<String>, Option<ResumeHint>),
}

/// The user's answer about an existing target.
enum Decision {
    Replace,
    Keep,
    Cancel,
}

/// What an existing target produced.
enum Question {
    /// The target may be replaced.
    Replace,
    /// The transfer must not write the target.
    Stop(End),
}

/// One running task and the ways it can be interrupted.
struct Job {
    id: TransferId,
    events: UnboundedSender<TransferEvent>,
    messages: UnboundedReceiver<TaskMessage>,
}

impl Job {
    fn report(&self, update: TransferUpdate) {
        let _ = self.events.send(TransferEvent {
            transfer: self.id,
            update,
        });
    }

    fn progress(&self, transferred: u64, total: Option<u64>) {
        self.report(TransferUpdate::Progress { transferred, total });
    }

    fn checkpoint(&self, hint: ResumeHint) {
        self.report(TransferUpdate::Checkpoint(hint));
    }

    /// Waits for the answer about an existing target.
    ///
    /// A cancel, and a channel that is gone because the session ended, mean the same
    /// thing here: nothing was written, so nothing is replaced.
    async fn decide(&mut self) -> Decision {
        match self.messages.recv().await {
            Some(TaskMessage::Decide(choice)) => {
                if choice.replaces() {
                    Decision::Replace
                } else {
                    Decision::Keep
                }
            }
            Some(TaskMessage::Cancel) | None => Decision::Cancel,
        }
    }

    /// Whether a cancel arrived while bytes were moving.
    ///
    /// It is asked between chunks rather than by racing the write, so a chunk that is
    /// already in flight is never abandoned half-sent.
    fn interrupted(&mut self) -> bool {
        loop {
            match self.messages.try_recv() {
                Ok(TaskMessage::Cancel) | Err(TryRecvError::Disconnected) => return true,
                // A decision that arrives after the question was answered is not a
                // cancellation: the transfer is already running.
                Ok(TaskMessage::Decide(_)) => {}
                Err(TryRecvError::Empty) => return false,
            }
        }
    }
}

/// Runs one transfer to its end and reports it.
async fn run(
    request: TransferRequest,
    commands: UnboundedSender<Command>,
    events: UnboundedSender<TransferEvent>,
    messages: UnboundedReceiver<TaskMessage>,
) {
    let mut job = Job {
        id: request.transfer,
        events,
        messages,
    };
    let session = match open(&commands).await {
        Ok(session) => session,
        Err(failure) => {
            job.report(TransferUpdate::Failed {
                failure,
                residue: None,
                resume: None,
            });
            return;
        }
    };
    let end = match request.direction {
        TransferDirection::Upload => upload(&session, &request, &mut job).await,
        TransferDirection::Download => download(&session, &request, &mut job).await,
    };
    match end {
        End::Completed(bytes) => job.report(TransferUpdate::Completed { bytes }),
        End::Failed(failure, residue, resume) => {
            job.report(TransferUpdate::Failed {
                failure,
                residue,
                resume,
            });
        }
        End::Cancelled(residue, resume) => {
            job.report(TransferUpdate::Cancelled { residue, resume });
        }
    }
    let _ = session.close().await;
}

/// Opens the transfer's own SFTP channel on the live connection.
async fn open(commands: &UnboundedSender<Command>) -> Result<SftpSession, TransferFailure> {
    let (reply, answer) = oneshot::channel();
    if commands.send(Command::Sftp(reply)).is_err() {
        return Err(TransferFailure::ConnectionLost);
    }
    let channel = match answer.await {
        Ok(Ok(channel)) => channel,
        Ok(Err(ChannelFailure::Unavailable { reason })) => {
            return Err(TransferFailure::Refused { reason });
        }
        Ok(Err(ChannelFailure::Lost { .. })) | Err(_) => {
            return Err(TransferFailure::ConnectionLost);
        }
    };
    let config = Config {
        request_timeout_secs: REQUEST_TIMEOUT_SECS,
        ..Config::default()
    };
    SftpSession::new_with_config(channel.into_stream(), config)
        .await
        .map_err(|error| match error {
            SftpError::Timeout => TransferFailure::ConnectionLost,
            other => TransferFailure::Refused {
                reason: other.to_string(),
            },
        })
}

/// Source identity captured before this attempt writes.
struct SourceIdentity {
    size: u64,
    modified_unix: Option<u64>,
    /// SHA-256 of the whole source, only when no mtime could be recorded.
    content_sha256: Option<String>,
}

/// Bytes confirmed for one partial file, and the checkpoint that names them.
struct Ledger {
    original: TransferId,
    direction: TransferDirection,
    source: String,
    destination: String,
    identity: SourceIdentity,
    generation: u64,
    offset: u64,
    hasher: Sha256,
    partial: String,
    record_path: PathBuf,
}

impl Ledger {
    fn fresh(
        id: TransferId,
        request: &TransferRequest,
        identity: SourceIdentity,
        partial: String,
    ) -> Self {
        Self {
            original: id,
            direction: request.direction,
            source: request.source.clone(),
            destination: request.destination.clone(),
            identity,
            generation: 1,
            offset: 0,
            hasher: Sha256::new(),
            partial,
            record_path: resume_record_path(id),
        }
    }

    fn note(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.offset += bytes.len() as u64;
    }

    fn prefix_hash(&self) -> String {
        hex_encode(&self.hasher.clone().finalize())
    }

    fn record(&self) -> PartialRecord {
        PartialRecord {
            transfer: self.original,
            direction: self.direction,
            source: self.source.clone(),
            destination: self.destination.clone(),
            source_size: self.identity.size,
            source_modified_unix: self.identity.modified_unix,
            content_sha256: self.identity.content_sha256.clone(),
            prefix_sha256: self.prefix_hash(),
            confirmed_offset: self.offset,
            generation: self.generation,
        }
    }

    fn hint(&self) -> ResumeHint {
        ResumeHint {
            partial: self.partial.clone(),
            record: self.record_path.display().to_string(),
            offset: self.offset,
            generation: self.generation,
        }
    }

    async fn persist(&self) -> bool {
        if self.offset == 0 {
            return false;
        }
        write_record(&self.record_path, &self.record()).await
    }

    async fn publish(&self, job: &Job) {
        if self.persist().await {
            job.checkpoint(self.hint());
        }
    }
}

/// A resume that already re-checked the source and the partial file.
struct Prepared<S, D> {
    source: S,
    destination: D,
    ledger: Ledger,
    written: u64,
}

/// Writes one local file into the remote directory, under a temporary name first.
async fn upload(session: &SftpSession, request: &TransferRequest, job: &mut Job) -> End {
    let local = PathBuf::from(&request.source);
    let identity = match local_identity(&local).await {
        Some(identity) => identity,
        None => return End::Failed(TransferFailure::SourceUnreadable, None, None),
    };
    let total = Some(identity.size);
    // A resume is refused before the overwrite question and before any write. The
    // refusal carries no progress, so the old offset cannot be mistaken for this attempt.
    let prepared = match prepare_upload(session, &local, &identity, request).await {
        Ok(prepared) => prepared,
        Err(end) => return end,
    };
    let target = request.destination.clone();
    let target_directory = parent_path(&target).unwrap_or_else(|| "/".to_owned());
    let existing = session.symlink_metadata(&target).await.is_ok();
    match ask_about_target(existing, total, job).await {
        Question::Replace => {}
        // The question itself wrote nothing. A resumed partial belongs to the earlier
        // task, so this attempt must not delete it on the way out.
        Question::Stop(end) => return end,
    }
    if request.resume.is_some()
        && local_identity(&local)
            .await
            .is_none_or(|again| !identity_matches(&identity, &again))
    {
        return End::Failed(
            TransferFailure::ResumeRefused {
                reason: ResumeRefusal::SourceChanged,
            },
            None,
            None,
        );
    }

    let (temporary, mut source, mut remote, mut ledger, mut written) = match prepared {
        Some(prepared) => {
            let written = prepared.written;
            job.progress(written, total);
            (
                prepared.ledger.partial.clone(),
                prepared.source,
                prepared.destination,
                prepared.ledger,
                written,
            )
        }
        None => {
            let temporary = remote_partial(&target, job.id);
            let source = match tokio::fs::File::open(&local).await {
                Ok(file) => file,
                Err(_) => return End::Failed(TransferFailure::SourceUnreadable, None, None),
            };
            let remote = match session.create(&temporary).await {
                Ok(file) => file,
                Err(error) => return End::Failed(failure_of(&error), None, None),
            };
            let ledger = Ledger::fresh(job.id, request, identity, temporary.clone());
            (temporary, source, remote, ledger, 0)
        }
    };

    let mut buffer = vec![0u8; CHUNK];
    let mut reported = Instant::now();
    loop {
        if job.interrupted() {
            drop(remote);
            let (residue, resume) = settle_remote(session, &ledger, &temporary, true).await;
            return End::Cancelled(residue, resume);
        }
        let read = match source.read(&mut buffer).await {
            Ok(read) => read,
            Err(_) => {
                drop(remote);
                let (residue, resume) =
                    settle_remote(session, &ledger, &temporary, ledger.offset > 0).await;
                return End::Failed(TransferFailure::SourceUnreadable, residue, resume);
            }
        };
        if read == 0 {
            break;
        }
        if let Err(error) = remote.write_all(&buffer[..read]).await {
            drop(remote);
            let failure = write_failure(session, &target_directory, &error).await;
            let (residue, resume) =
                settle_remote(session, &ledger, &temporary, ledger.offset > 0).await;
            return End::Failed(failure, residue, resume);
        }
        // The server has acknowledged the bytes. Only then are they confirmed.
        if let Err(error) = remote.flush().await {
            drop(remote);
            let failure = write_failure(session, &target_directory, &error).await;
            let (residue, resume) =
                settle_remote(session, &ledger, &temporary, ledger.offset > 0).await;
            return End::Failed(failure, residue, resume);
        }
        ledger.note(&buffer[..read]);
        written += read as u64;
        if reported.elapsed() >= PROGRESS_INTERVAL {
            ledger.publish(job).await;
            job.progress(written, total);
            reported = Instant::now();
        }
    }
    ledger.publish(job).await;
    job.progress(written, total);
    if let Err(error) = remote.shutdown().await {
        let failure = write_failure(session, &target_directory, &error).await;
        let (residue, resume) =
            settle_remote(session, &ledger, &temporary, ledger.offset > 0).await;
        return End::Failed(failure, residue, resume);
    }

    match session.metadata(&temporary).await {
        Ok(metadata) if metadata.size.is_some_and(|size| size == written) => {}
        Ok(_) => {
            let (residue, _) = settle_remote(session, &ledger, &temporary, false).await;
            return End::Failed(TransferFailure::FinalizeFailed, residue, None);
        }
        Err(error) => {
            let (residue, resume) =
                settle_remote(session, &ledger, &temporary, ledger.offset > 0).await;
            return End::Failed(failure_of(&error), residue, resume);
        }
    }

    if existing && session.remove_file(&target).await.is_err() {
        let (residue, resume) = settle_remote(session, &ledger, &temporary, true).await;
        return End::Failed(TransferFailure::FinalizeFailed, residue, resume);
    }
    match session.rename(&temporary, &target).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&ledger.record_path).await;
            End::Completed(written)
        }
        Err(_) => {
            let (residue, resume) = settle_remote(session, &ledger, &temporary, true).await;
            End::Failed(TransferFailure::FinalizeFailed, residue, resume)
        }
    }
}

/// Reads one remote file into the local directory, under a temporary name first.
async fn download(session: &SftpSession, request: &TransferRequest, job: &mut Job) -> End {
    let source_path = request.source.clone();
    let identity = match remote_identity(session, &source_path).await {
        Ok(identity) => identity,
        Err(failure) => return End::Failed(failure, None, None),
    };
    let total = Some(identity.size);
    let prepared = match prepare_download(session, &identity, request).await {
        Ok(prepared) => prepared,
        Err(end) => return end,
    };
    let target = PathBuf::from(&request.destination);
    let existing = tokio::fs::metadata(&target).await.is_ok();
    match ask_about_target(existing, total, job).await {
        Question::Replace => {}
        Question::Stop(end) => return end,
    }
    if request.resume.is_some() {
        match remote_identity(session, &source_path).await {
            Ok(again) if identity_matches(&identity, &again) => {}
            _ => {
                return End::Failed(
                    TransferFailure::ResumeRefused {
                        reason: ResumeRefusal::SourceChanged,
                    },
                    None,
                    None,
                );
            }
        }
    }

    let (temporary, mut remote, mut destination, mut ledger, mut written) = match prepared {
        Some(prepared) => {
            let written = prepared.written;
            job.progress(written, total);
            (
                PathBuf::from(&prepared.ledger.partial),
                prepared.source,
                prepared.destination,
                prepared.ledger,
                written,
            )
        }
        None => {
            let temporary = local_partial(&target, job.id);
            let destination = match tokio::fs::File::create(&temporary).await {
                Ok(file) => file,
                Err(_) => return End::Failed(TransferFailure::DestinationUnwritable, None, None),
            };
            let remote = match session.open(&source_path).await {
                Ok(file) => file,
                Err(error) => {
                    let residue = discard_local(&temporary).await;
                    return End::Failed(failure_of(&error), residue, None);
                }
            };
            let ledger = Ledger::fresh(job.id, request, identity, temporary.display().to_string());
            (temporary, remote, destination, ledger, 0)
        }
    };

    let mut buffer = vec![0u8; CHUNK];
    let mut reported = Instant::now();
    loop {
        if job.interrupted() {
            drop(destination);
            let (residue, resume) = settle_local(&ledger, &temporary, true).await;
            return End::Cancelled(residue, resume);
        }
        let read = match remote.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                drop(destination);
                let (residue, resume) = settle_local(&ledger, &temporary, ledger.offset > 0).await;
                return End::Failed(io_failure(&error), residue, resume);
            }
        };
        if read == 0 {
            break;
        }
        if destination.write_all(&buffer[..read]).await.is_err() {
            drop(destination);
            let (residue, resume) = settle_local(&ledger, &temporary, ledger.offset > 0).await;
            return End::Failed(TransferFailure::DestinationUnwritable, residue, resume);
        }
        if destination.flush().await.is_err() {
            drop(destination);
            let (residue, resume) = settle_local(&ledger, &temporary, ledger.offset > 0).await;
            return End::Failed(TransferFailure::DestinationUnwritable, residue, resume);
        }
        ledger.note(&buffer[..read]);
        written += read as u64;
        if reported.elapsed() >= PROGRESS_INTERVAL {
            ledger.publish(job).await;
            job.progress(written, total);
            reported = Instant::now();
        }
    }
    ledger.publish(job).await;
    job.progress(written, total);
    if destination.flush().await.is_err() || destination.set_len(written).await.is_err() {
        drop(destination);
        let (residue, resume) = settle_local(&ledger, &temporary, false).await;
        return End::Failed(TransferFailure::DestinationUnwritable, residue, resume);
    }
    drop(destination);

    if existing && tokio::fs::remove_file(&target).await.is_err() {
        let (residue, resume) = settle_local(&ledger, &temporary, true).await;
        return End::Failed(TransferFailure::FinalizeFailed, residue, resume);
    }
    match tokio::fs::rename(&temporary, &target).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&ledger.record_path).await;
            End::Completed(written)
        }
        Err(_) => {
            let (residue, resume) = settle_local(&ledger, &temporary, true).await;
            End::Failed(TransferFailure::FinalizeFailed, residue, resume)
        }
    }
}

/// Asks about an existing target when there is one, and reports the start otherwise.
async fn ask_about_target(existing: bool, total: Option<u64>, job: &mut Job) -> Question {
    if !existing {
        job.report(TransferUpdate::Started);
        return Question::Replace;
    }
    job.report(TransferUpdate::TargetExists { total });
    match job.decide().await {
        Decision::Replace => {
            job.report(TransferUpdate::OverwriteConfirmed);
            Question::Replace
        }
        // Keeping the existing file is the transfer ending without writing it: the
        // target is untouched, and the report says the transfer was cancelled rather
        // than completed.
        Decision::Keep | Decision::Cancel => Question::Stop(End::Cancelled(None, None)),
    }
}

async fn prepare_upload(
    session: &SftpSession,
    local: &Path,
    identity: &SourceIdentity,
    request: &TransferRequest,
) -> Result<Option<Prepared<tokio::fs::File, russh_sftp::client::fs::File>>, End> {
    let Some(hint) = &request.resume else {
        return Ok(None);
    };
    match open_upload_resume(session, local, identity, request, hint).await {
        Ok(prepared) => Ok(Some(prepared)),
        Err(reason) => {
            if matches!(
                reason,
                ResumeRefusal::SourceChanged | ResumeRefusal::PartialDamaged
            ) {
                let _ = discard_remote(session, &hint.partial).await;
                let _ = tokio::fs::remove_file(&hint.record).await;
            }
            Err(End::Failed(
                TransferFailure::ResumeRefused { reason },
                None,
                None,
            ))
        }
    }
}

async fn open_upload_resume(
    session: &SftpSession,
    local: &Path,
    identity: &SourceIdentity,
    request: &TransferRequest,
    hint: &ResumeHint,
) -> Result<Prepared<tokio::fs::File, russh_sftp::client::fs::File>, ResumeRefusal> {
    let record = load_matching_record(hint, request).await?;
    assess_identity(
        &record,
        identity.size,
        identity.modified_unix,
        identity.content_sha256.as_deref(),
    )?;
    let partial_len = remote_len(session, &hint.partial).await?;
    let partial_hash = hash_remote_prefix(session, &hint.partial, record.confirmed_offset)
        .await
        .ok_or(ResumeRefusal::PartialDamaged)?;
    let mut source = tokio::fs::File::open(local)
        .await
        .map_err(|_| ResumeRefusal::Unverifiable)?;
    let mut hasher = Sha256::new();
    if !absorb(&mut source, &mut hasher, record.confirmed_offset).await {
        return Err(ResumeRefusal::Unverifiable);
    }
    let source_hash = hex_encode(&hasher.clone().finalize());
    assess_prefix(&record, partial_len, &partial_hash, &source_hash)?;
    let mut remote = match session
        .open_with_flags(&hint.partial, OpenFlags::WRITE)
        .await
    {
        Ok(file) => file,
        Err(_) => {
            return Err(if session.metadata(&hint.partial).await.is_ok() {
                ResumeRefusal::Unsupported
            } else {
                ResumeRefusal::PartialDamaged
            });
        }
    };
    if remote
        .seek(SeekFrom::Start(record.confirmed_offset))
        .await
        .is_err()
    {
        return Err(ResumeRefusal::Unsupported);
    }
    let mut ledger = Ledger::fresh(
        record.transfer,
        request,
        identity.clone(),
        hint.partial.clone(),
    );
    ledger.generation = record.generation;
    ledger.offset = record.confirmed_offset;
    ledger.hasher = hasher;
    ledger.record_path = PathBuf::from(&hint.record);
    ledger.partial = hint.partial.clone();
    Ok(Prepared {
        source,
        destination: remote,
        written: record.confirmed_offset,
        ledger,
    })
}

async fn prepare_download(
    session: &SftpSession,
    identity: &SourceIdentity,
    request: &TransferRequest,
) -> Result<Option<Prepared<russh_sftp::client::fs::File, tokio::fs::File>>, End> {
    let Some(hint) = &request.resume else {
        return Ok(None);
    };
    match open_download_resume(session, identity, request, hint).await {
        Ok(prepared) => Ok(Some(prepared)),
        Err(reason) => {
            if matches!(
                reason,
                ResumeRefusal::SourceChanged | ResumeRefusal::PartialDamaged
            ) {
                let _ = discard_local(Path::new(&hint.partial)).await;
                let _ = tokio::fs::remove_file(&hint.record).await;
            }
            Err(End::Failed(
                TransferFailure::ResumeRefused { reason },
                None,
                None,
            ))
        }
    }
}

async fn open_download_resume(
    session: &SftpSession,
    identity: &SourceIdentity,
    request: &TransferRequest,
    hint: &ResumeHint,
) -> Result<Prepared<russh_sftp::client::fs::File, tokio::fs::File>, ResumeRefusal> {
    let record = load_matching_record(hint, request).await?;
    assess_identity(
        &record,
        identity.size,
        identity.modified_unix,
        identity.content_sha256.as_deref(),
    )?;
    let partial_path = PathBuf::from(&hint.partial);
    let partial_len = tokio::fs::metadata(&partial_path)
        .await
        .map_err(|_| ResumeRefusal::PartialDamaged)?
        .len();
    let partial_hash = hash_local_prefix(&partial_path, record.confirmed_offset)
        .await
        .ok_or(ResumeRefusal::PartialDamaged)?;
    let mut remote = session
        .open(&request.source)
        .await
        .map_err(|_| ResumeRefusal::Unverifiable)?;
    let mut hasher = Sha256::new();
    if !absorb(&mut remote, &mut hasher, record.confirmed_offset).await {
        return Err(ResumeRefusal::Unverifiable);
    }
    let source_hash = hex_encode(&hasher.clone().finalize());
    assess_prefix(&record, partial_len, &partial_hash, &source_hash)?;
    let mut destination = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .open(&partial_path)
        .await
        .map_err(|_| ResumeRefusal::Unsupported)?;
    if destination
        .seek(SeekFrom::Start(record.confirmed_offset))
        .await
        .is_err()
    {
        return Err(ResumeRefusal::Unsupported);
    }
    let mut ledger = Ledger::fresh(
        record.transfer,
        request,
        identity.clone(),
        hint.partial.clone(),
    );
    ledger.generation = record.generation;
    ledger.offset = record.confirmed_offset;
    ledger.hasher = hasher;
    ledger.record_path = PathBuf::from(&hint.record);
    Ok(Prepared {
        source: remote,
        destination,
        written: record.confirmed_offset,
        ledger,
    })
}

async fn load_matching_record(
    hint: &ResumeHint,
    request: &TransferRequest,
) -> Result<PartialRecord, ResumeRefusal> {
    let record = read_record(Path::new(&hint.record)).await?;
    assess_hint(&record, hint, request)?;
    Ok(record)
}

fn assess_hint(
    record: &PartialRecord,
    hint: &ResumeHint,
    request: &TransferRequest,
) -> Result<(), ResumeRefusal> {
    if record.direction != request.direction
        || record.source != request.source
        || record.destination != request.destination
        || record.generation != hint.generation
        || record.confirmed_offset != hint.offset
        || record.prefix_sha256.len() != 64
        || hint.offset == 0
        || hint.partial.is_empty()
    {
        return Err(ResumeRefusal::Unverifiable);
    }
    if record.confirmed_offset > record.source_size {
        return Err(ResumeRefusal::PartialDamaged);
    }
    Ok(())
}

fn assess_identity(
    record: &PartialRecord,
    size: u64,
    modified_unix: Option<u64>,
    content_sha256: Option<&str>,
) -> Result<(), ResumeRefusal> {
    if size != record.source_size {
        return Err(ResumeRefusal::SourceChanged);
    }
    match record.source_modified_unix {
        Some(expected) => match modified_unix {
            Some(actual) if actual == expected => Ok(()),
            Some(_) => Err(ResumeRefusal::SourceChanged),
            None => Err(ResumeRefusal::Unverifiable),
        },
        None => match (record.content_sha256.as_deref(), content_sha256) {
            (Some(expected), Some(actual)) if expected == actual => Ok(()),
            (Some(_), Some(_)) => Err(ResumeRefusal::SourceChanged),
            _ => Err(ResumeRefusal::Unverifiable),
        },
    }
}

fn assess_prefix(
    record: &PartialRecord,
    partial_len: u64,
    partial_hash: &str,
    source_hash: &str,
) -> Result<(), ResumeRefusal> {
    if partial_len < record.confirmed_offset || partial_len > record.source_size {
        return Err(ResumeRefusal::PartialDamaged);
    }
    if partial_hash != record.prefix_sha256 {
        return Err(ResumeRefusal::PartialDamaged);
    }
    if source_hash != record.prefix_sha256 {
        return Err(ResumeRefusal::SourceChanged);
    }
    Ok(())
}

fn identity_matches(before: &SourceIdentity, after: &SourceIdentity) -> bool {
    before.size == after.size
        && before.modified_unix == after.modified_unix
        && before.content_sha256 == after.content_sha256
}

async fn local_identity(path: &Path) -> Option<SourceIdentity> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified_unix = metadata.modified().ok().and_then(unix_secs);
    let content_sha256 = if modified_unix.is_none() {
        Some(hash_local_all(path).await?)
    } else {
        None
    };
    Some(SourceIdentity {
        size: metadata.len(),
        modified_unix,
        content_sha256,
    })
}

async fn remote_identity(
    session: &SftpSession,
    path: &str,
) -> Result<SourceIdentity, TransferFailure> {
    let metadata = match session.metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) => return Err(failure_of(&error)),
    };
    if metadata.is_dir() {
        return Err(TransferFailure::Refused {
            reason: "the source is a directory, not a file".to_owned(),
        });
    }
    let Some(size) = metadata.size else {
        return Err(TransferFailure::Refused {
            reason: "the server did not report the source size".to_owned(),
        });
    };
    let modified_unix = metadata.mtime.map(u64::from);
    let content_sha256 = if modified_unix.is_none() {
        Some(
            hash_remote_all(session, path)
                .await
                .ok_or(TransferFailure::SourceUnreadable)?,
        )
    } else {
        None
    };
    Ok(SourceIdentity {
        size,
        modified_unix,
        content_sha256,
    })
}

async fn remote_len(session: &SftpSession, path: &str) -> Result<u64, ResumeRefusal> {
    let metadata = session
        .metadata(path)
        .await
        .map_err(|_| ResumeRefusal::PartialDamaged)?;
    metadata.size.ok_or(ResumeRefusal::PartialDamaged)
}

async fn hash_remote_prefix(session: &SftpSession, path: &str, len: u64) -> Option<String> {
    let mut file = session.open(path).await.ok()?;
    let mut hasher = Sha256::new();
    if !absorb(&mut file, &mut hasher, len).await {
        return None;
    }
    Some(hex_encode(&hasher.finalize()))
}

async fn hash_remote_all(session: &SftpSession, path: &str) -> Option<String> {
    let mut file = session.open(path).await.ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer).await.ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hex_encode(&hasher.finalize()))
}

async fn hash_local_prefix(path: &Path, len: u64) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut hasher = Sha256::new();
    if !absorb(&mut file, &mut hasher, len).await {
        return None;
    }
    Some(hex_encode(&hasher.finalize()))
}

async fn hash_local_all(path: &Path) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer).await.ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hex_encode(&hasher.finalize()))
}

async fn absorb(reader: &mut (impl AsyncReadExt + Unpin), hasher: &mut Sha256, len: u64) -> bool {
    let mut left = len;
    let mut buffer = vec![0u8; CHUNK];
    while left > 0 {
        let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
        let read = match reader.read(&mut buffer[..want]).await {
            Ok(read) => read,
            Err(_) => return false,
        };
        if read == 0 {
            return false;
        }
        hasher.update(&buffer[..read]);
        left -= read as u64;
    }
    true
}

fn resume_record_path(id: TransferId) -> PathBuf {
    std::env::temp_dir()
        .join("shellune-transfers")
        .join(format!("{id}.json"))
}

async fn write_record(path: &Path, record: &PartialRecord) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if tokio::fs::create_dir_all(parent).await.is_err() {
        return false;
    }
    let bytes = match serde_json::to_vec(record) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let temporary = path.with_extension("tmp");
    if tokio::fs::write(&temporary, bytes).await.is_err() {
        return false;
    }
    tokio::fs::rename(&temporary, path).await.is_ok()
}

async fn read_record(path: &Path) -> Result<PartialRecord, ResumeRefusal> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| ResumeRefusal::Unverifiable)?;
    serde_json::from_slice(&bytes).map_err(|_| ResumeRefusal::Unverifiable)
}

/// Keeps a remote partial when its prefix was confirmed, and removes it otherwise.
async fn settle_remote(
    session: &SftpSession,
    ledger: &Ledger,
    partial: &str,
    keep: bool,
) -> (Option<String>, Option<ResumeHint>) {
    if keep && ledger.offset > 0 {
        if ledger.persist().await {
            return (Some(partial.to_owned()), Some(ledger.hint()));
        }
        return (Some(partial.to_owned()), None);
    }
    let residue = discard_remote(session, partial).await;
    let _ = tokio::fs::remove_file(&ledger.record_path).await;
    (residue, None)
}

/// Keeps a local partial when its prefix was confirmed, and removes it otherwise.
async fn settle_local(
    ledger: &Ledger,
    partial: &Path,
    keep: bool,
) -> (Option<String>, Option<ResumeHint>) {
    if keep && ledger.offset > 0 {
        if ledger.persist().await {
            return (Some(partial.display().to_string()), Some(ledger.hint()));
        }
        return (Some(partial.display().to_string()), None);
    }
    let residue = discard_local(partial).await;
    let _ = tokio::fs::remove_file(&ledger.record_path).await;
    (residue, None)
}

fn unix_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

impl Clone for SourceIdentity {
    fn clone(&self) -> Self {
        Self {
            size: self.size,
            modified_unix: self.modified_unix,
            content_sha256: self.content_sha256.clone(),
        }
    }
}

/// The temporary name a target's content is written under, in its own directory.
///
/// The path is shellune-core's answer ([shellune_core::partial_path]), not a second
/// opinion: the window derives the leftover of a task whose worker died from the same
/// rule, and two derivations that disagree would name two different files.
fn remote_partial(target: &str, id: TransferId) -> String {
    shellune_core::partial_path(TransferDirection::Upload, target, id)
}

/// The temporary name a local target is written under, beside the target.
fn local_partial(target: &Path, id: TransferId) -> PathBuf {
    PathBuf::from(shellune_core::partial_path(
        TransferDirection::Download,
        &target.to_string_lossy(),
        id,
    ))
}

/// Removes a partial remote file, naming it when it could not be removed.
async fn discard_remote(session: &SftpSession, path: &str) -> Option<String> {
    match session.remove_file(path).await {
        Ok(()) => None,
        Err(_) => Some(path.to_owned()),
    }
}

/// Removes a partial local file, naming it when it could not be removed.
async fn discard_local(path: &Path) -> Option<String> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => None,
        Err(_) => Some(path.display().to_string()),
    }
}

/// The failure a write error describes, asking the server when the code is generic.
///
/// OpenSSH answers a write onto a full disk with the same generic `Failure` it uses for
/// many refusals, so the error text alone cannot tell the user what to do. The server can
/// be asked how much room is left in the directory being written (the
/// `statvfs@openssh.com` extension); an answer of "nothing" is what makes this report a
/// full disk instead of a lost connection.
async fn write_failure(
    session: &SftpSession,
    directory: &str,
    error: &std::io::Error,
) -> TransferFailure {
    if is_out_of_space(&error.to_string()) {
        return TransferFailure::NoSpace;
    }
    if server_is_full(session, directory).await {
        return TransferFailure::NoSpace;
    }
    io_failure(error)
}

/// Whether the server reports that the file system holding a path has no room left.
async fn server_is_full(session: &SftpSession, directory: &str) -> bool {
    match session.fs_info(directory).await {
        Ok(Some(info)) => info.blocks_avail == 0 || info.blocks_free == 0,
        _ => false,
    }
}

/// Whether a server message describes a full disk or an exhausted quota.
fn is_out_of_space(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("no space") || message.contains("quota") || message.contains("disk full")
}

/// The failure one SFTP answer describes.
fn failure_of(error: &SftpError) -> TransferFailure {
    match error {
        SftpError::Status(status) => {
            if is_out_of_space(&status.error_message) {
                return TransferFailure::NoSpace;
            }
            match status.status_code {
                StatusCode::NoSuchFile => TransferFailure::NotFound,
                StatusCode::PermissionDenied => TransferFailure::PermissionDenied,
                StatusCode::NoConnection | StatusCode::ConnectionLost => {
                    TransferFailure::ConnectionLost
                }
                other => TransferFailure::Refused {
                    reason: format!("{} ({other})", status.error_message),
                },
            }
        }
        // A request that was never answered is a connection that is no longer usable;
        // the channel is opened again by the next transfer, which is why this does not
        // close anything else.
        SftpError::Timeout => TransferFailure::ConnectionLost,
        SftpError::IO(reason) | SftpError::Limited(reason) => {
            if is_out_of_space(reason) {
                TransferFailure::NoSpace
            } else {
                TransferFailure::ConnectionLost
            }
        }
        other => TransferFailure::Refused {
            reason: other.to_string(),
        },
    }
}

/// The failure one stream error describes.
fn io_failure(error: &std::io::Error) -> TransferFailure {
    if is_out_of_space(&error.to_string()) {
        TransferFailure::NoSpace
    } else {
        TransferFailure::ConnectionLost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temporary_name_sits_beside_its_target_and_names_itself() {
        let id = TransferId::new();
        assert_eq!(
            remote_partial("/home/deploy/report.txt", id),
            format!("/home/deploy/{PARTIAL_PREFIX}{id}")
        );
        assert_eq!(
            remote_partial("/report.txt", id),
            format!("/{PARTIAL_PREFIX}{id}")
        );

        let local = local_partial(Path::new("C:/work/report.txt"), id);
        assert_eq!(
            local,
            Path::new("C:/work").join(format!("{PARTIAL_PREFIX}{id}"))
        );
        assert!(
            !local.to_string_lossy().contains("report"),
            "the temporary file must not be mistaken for the target"
        );
    }

    #[test]
    fn a_chinese_name_with_spaces_keeps_its_exact_characters() {
        let id = TransferId::new();
        let target = "/home/deploy/文档 与 空格/报告 终稿.txt";
        let partial = remote_partial(target, id);
        assert!(partial.starts_with("/home/deploy/文档 与 空格/"));
        assert!(partial.ends_with(&format!("{PARTIAL_PREFIX}{id}")));
    }

    #[test]
    fn a_full_disk_is_told_apart_from_other_refusals() {
        let status = russh_sftp::protocol::Status {
            id: 0,
            status_code: StatusCode::Failure,
            error_message: "Failure: No space left on device".to_owned(),
            language_tag: String::new(),
        };
        assert_eq!(
            failure_of(&SftpError::Status(status)),
            TransferFailure::NoSpace
        );

        let refused = russh_sftp::protocol::Status {
            id: 0,
            status_code: StatusCode::PermissionDenied,
            error_message: "Permission denied".to_owned(),
            language_tag: String::new(),
        };
        assert_eq!(
            failure_of(&SftpError::Status(refused)),
            TransferFailure::PermissionDenied
        );

        let missing = russh_sftp::protocol::Status {
            id: 0,
            status_code: StatusCode::NoSuchFile,
            error_message: "No such file".to_owned(),
            language_tag: String::new(),
        };
        assert_eq!(
            failure_of(&SftpError::Status(missing)),
            TransferFailure::NotFound
        );
    }

    #[test]
    fn a_link_stops_accepting_commands_once_it_is_closed() {
        let (commands, _receiver) = unbounded_channel();
        let (_events, events) = unbounded_channel();
        let mut link = TransferLink::new(commands, events);
        assert!(link.send(TransferCommand::Start(TransferRequest {
            transfer: TransferId::new(),
            direction: TransferDirection::Upload,
            source: "a.txt".to_owned(),
            destination: "/home/deploy/a.txt".to_owned(),
            resume: None,
        })));
        link.close();
        assert!(link.is_closed());
        assert!(
            !link.send(TransferCommand::Cancel {
                transfer: TransferId::new()
            }),
            "a closed link must not promise to serve a command"
        );
        assert!(link.drain().is_empty());
    }

    #[test]
    fn a_request_is_built_from_the_task_it_belongs_to() {
        let task = Transfer::queued(
            shellune_core::SessionId::new(),
            TransferDirection::Download,
            "/home/deploy/说明 文档.txt",
            "C:/work/说明 文档.txt",
        );
        let request = TransferRequest::for_transfer(&task);
        assert_eq!(request.transfer, task.id());
        assert_eq!(request.direction, TransferDirection::Download);
        assert_eq!(request.source, "/home/deploy/说明 文档.txt");
        assert_eq!(request.destination, "C:/work/说明 文档.txt");
        assert!(request.resume.is_none());
    }

    fn record(offset: u64, size: u64) -> PartialRecord {
        PartialRecord {
            transfer: TransferId::new(),
            direction: TransferDirection::Upload,
            source: "C:/work/report.txt".to_owned(),
            destination: "/home/deploy/report.txt".to_owned(),
            source_size: size,
            source_modified_unix: Some(10),
            content_sha256: None,
            prefix_sha256: "ab".repeat(32),
            confirmed_offset: offset,
            generation: 1,
        }
    }

    fn hint_for(record: &PartialRecord) -> ResumeHint {
        ResumeHint {
            partial: "/home/deploy/.shellune-part-x".to_owned(),
            record: "C:/temp/x.json".to_owned(),
            offset: record.confirmed_offset,
            generation: record.generation,
        }
    }

    fn request_for(record: &PartialRecord) -> TransferRequest {
        TransferRequest {
            transfer: TransferId::new(),
            direction: record.direction,
            source: record.source.clone(),
            destination: record.destination.clone(),
            resume: Some(hint_for(record)),
        }
    }

    #[test]
    fn a_checkpoint_rejects_a_source_that_changed_and_a_prefix_that_does_not_match() {
        let record = record(4, 8);
        let request = request_for(&record);
        let hint = hint_for(&record);
        assert_eq!(assess_hint(&record, &hint, &request), Ok(()));
        assert_eq!(
            assess_identity(&record, 8, Some(10), None),
            Ok(()),
            "the same size and mtime still describe this source"
        );
        assert_eq!(
            assess_identity(&record, 9, Some(10), None),
            Err(ResumeRefusal::SourceChanged)
        );
        assert_eq!(
            assess_identity(&record, 8, Some(11), None),
            Err(ResumeRefusal::SourceChanged)
        );
        assert_eq!(
            assess_identity(&record, 8, None, None),
            Err(ResumeRefusal::Unverifiable),
            "a recorded mtime that can no longer be read is not a resume"
        );
        let hash = record.prefix_sha256.clone();
        assert_eq!(assess_prefix(&record, 4, &hash, &hash), Ok(()));
        assert_eq!(
            assess_prefix(&record, 3, &hash, &hash),
            Err(ResumeRefusal::PartialDamaged),
            "a short partial file is damaged"
        );
        assert_eq!(
            assess_prefix(&record, 4, "ff", &hash),
            Err(ResumeRefusal::PartialDamaged)
        );
        assert_eq!(
            assess_prefix(&record, 4, &hash, "ee"),
            Err(ResumeRefusal::SourceChanged),
            "a prefix that is not the source's prefix is not resumed"
        );
    }

    #[test]
    fn a_checkpoint_without_a_time_needs_the_content_hash() {
        let mut record = record(4, 8);
        record.source_modified_unix = None;
        record.content_sha256 = Some("cd".repeat(32));
        assert_eq!(
            assess_identity(&record, 8, None, Some(&"cd".repeat(32))),
            Ok(())
        );
        assert_eq!(
            assess_identity(&record, 8, None, Some(&"00".repeat(32))),
            Err(ResumeRefusal::SourceChanged)
        );
        assert_eq!(
            assess_identity(&record, 8, None, None),
            Err(ResumeRefusal::Unverifiable)
        );
    }
}
