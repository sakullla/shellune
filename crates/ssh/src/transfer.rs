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
//! * a cancellation or failure removes the temporary file and says so when it could
//!   not, because a partial file nobody mentions is worse than one that is named. The
//!   one case that does not remove it is a replacement that failed after the old target
//!   was already gone: there the completed copy is the only one left, so it is kept and
//!   named instead;
//! * every event carries the task's [`TransferId`], so a message from an attempt that
//!   already ended cannot make a newer task look finished. Retrying starts a new task
//!   from zero bytes: nothing here resumes a previous attempt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{Config, SftpSession};
use russh_sftp::protocol::StatusCode;
use shellune_core::{
    OverwriteChoice, Transfer, TransferDirection, TransferEvent, TransferFailure, TransferId,
    TransferUpdate, parent_path,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    /// The transfer stopped; the optional path is a partial file that stayed behind.
    Failed(TransferFailure, Option<String>),
    /// The user stopped it; the optional path is a partial file that stayed behind.
    Cancelled(Option<String>),
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
        End::Failed(failure, residue) => job.report(TransferUpdate::Failed { failure, residue }),
        End::Cancelled(residue) => job.report(TransferUpdate::Cancelled { residue }),
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

/// Writes one local file into the remote directory, under a temporary name first.
async fn upload(session: &SftpSession, request: &TransferRequest, job: &mut Job) -> End {
    let local = PathBuf::from(&request.source);
    let total = match tokio::fs::metadata(&local).await {
        Ok(metadata) if metadata.is_file() => Some(metadata.len()),
        Ok(_) | Err(_) => return End::Failed(TransferFailure::SourceUnreadable, None),
    };
    let target = request.destination.clone();
    // The directory is what the server is asked about when a write fails for a reason it
    // did not name: free space belongs to a file system, not to a file.
    let target_directory = parent_path(&target).unwrap_or_else(|| "/".to_owned());
    // The question is asked before anything is created, so a refusal of it writes
    // nothing at all.
    let existing = session.symlink_metadata(&target).await.is_ok();
    match ask_about_target(existing, total, job).await {
        Question::Replace => {}
        Question::Stop(end) => return end,
    }

    let temporary = remote_partial(&target, job.id);
    let mut source = match tokio::fs::File::open(&local).await {
        Ok(file) => file,
        Err(_) => return End::Failed(TransferFailure::SourceUnreadable, None),
    };
    let mut remote = match session.create(&temporary).await {
        Ok(file) => file,
        Err(error) => return End::Failed(failure_of(&error), None),
    };

    let mut written = 0u64;
    let mut buffer = vec![0u8; CHUNK];
    let mut reported = Instant::now();
    loop {
        if job.interrupted() {
            drop(remote);
            return End::Cancelled(discard_remote(session, &temporary).await);
        }
        let read = match source.read(&mut buffer).await {
            Ok(read) => read,
            Err(_) => {
                drop(remote);
                return End::Failed(
                    TransferFailure::SourceUnreadable,
                    discard_remote(session, &temporary).await,
                );
            }
        };
        if read == 0 {
            break;
        }
        if let Err(error) = remote.write_all(&buffer[..read]).await {
            drop(remote);
            let failure = write_failure(session, &target_directory, &error).await;
            return End::Failed(failure, discard_remote(session, &temporary).await);
        }
        written += read as u64;
        if reported.elapsed() >= PROGRESS_INTERVAL {
            job.progress(written, total);
            reported = Instant::now();
        }
    }
    job.progress(written, total);
    // A write onto a full disk can also surface when the handle is closed, because that
    // is where the last queued write is answered.
    if let Err(error) = remote.shutdown().await {
        let failure = write_failure(session, &target_directory, &error).await;
        return End::Failed(failure, discard_remote(session, &temporary).await);
    }

    // The temporary file is the only thing that was written so far, so it is asked what
    // it holds: a close that lost bytes must not become a rename that replaces a file.
    match session.metadata(&temporary).await {
        Ok(metadata) if metadata.size.is_some_and(|size| size == written) => {}
        Ok(_) => {
            return End::Failed(
                TransferFailure::FinalizeFailed,
                discard_remote(session, &temporary).await,
            );
        }
        Err(error) => {
            return End::Failed(
                failure_of(&error),
                discard_remote(session, &temporary).await,
            );
        }
    }

    if existing {
        // SFTP's rename does not replace an existing name on every server, so the old
        // target is removed first. Doing it now - after the bytes are complete - keeps
        // the window in which the target is missing as small as it can be.
        //
        // A target that cannot be removed - a directory, or a name the server refuses to
        // touch - is the final replacement failing: the temporary file is complete, the
        // target is not what the user asked for, and only the server can say what it now
        // holds. The complete copy stays where it is and is named as the residue: it is
        // the only good copy left, so deleting it would lose both.
        if session.remove_file(&target).await.is_err() {
            return End::Failed(TransferFailure::FinalizeFailed, Some(temporary));
        }
    }
    match session.rename(&temporary, &target).await {
        Ok(()) => End::Completed(written),
        // The target was already removed, so both copies would be lost by discarding
        // this one: the bytes are complete, they simply never got the name the user
        // asked for. It is kept and reported instead.
        Err(_) => End::Failed(TransferFailure::FinalizeFailed, Some(temporary)),
    }
}

/// Reads one remote file into the local directory, under a temporary name first.
async fn download(session: &SftpSession, request: &TransferRequest, job: &mut Job) -> End {
    let source = request.source.clone();
    let total = match session.metadata(&source).await {
        Ok(metadata) if metadata.is_dir() => {
            return End::Failed(
                TransferFailure::Refused {
                    reason: "the source is a directory, not a file".to_owned(),
                },
                None,
            );
        }
        Ok(metadata) => metadata.size,
        Err(error) => return End::Failed(failure_of(&error), None),
    };
    let target = PathBuf::from(&request.destination);
    let existing = tokio::fs::metadata(&target).await.is_ok();
    match ask_about_target(existing, total, job).await {
        Question::Replace => {}
        Question::Stop(end) => return end,
    }

    let temporary = local_partial(&target, job.id);
    let mut destination = match tokio::fs::File::create(&temporary).await {
        Ok(file) => file,
        Err(_) => return End::Failed(TransferFailure::DestinationUnwritable, None),
    };
    let mut remote = match session.open(&source).await {
        Ok(file) => file,
        Err(error) => {
            drop(destination);
            return End::Failed(failure_of(&error), discard_local(&temporary).await);
        }
    };

    let mut written = 0u64;
    let mut buffer = vec![0u8; CHUNK];
    let mut reported = Instant::now();
    loop {
        if job.interrupted() {
            drop(destination);
            return End::Cancelled(discard_local(&temporary).await);
        }
        let read = match remote.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                drop(destination);
                return End::Failed(io_failure(&error), discard_local(&temporary).await);
            }
        };
        if read == 0 {
            break;
        }
        if destination.write_all(&buffer[..read]).await.is_err() {
            drop(destination);
            return End::Failed(
                TransferFailure::DestinationUnwritable,
                discard_local(&temporary).await,
            );
        }
        written += read as u64;
        if reported.elapsed() >= PROGRESS_INTERVAL {
            job.progress(written, total);
            reported = Instant::now();
        }
    }
    job.progress(written, total);
    if destination.flush().await.is_err() {
        drop(destination);
        return End::Failed(
            TransferFailure::DestinationUnwritable,
            discard_local(&temporary).await,
        );
    }
    // The handle is closed before the rename: a platform that refuses to replace a file
    // that is still open would otherwise report a finalize failure for a finished copy.
    drop(destination);

    // As on the upload side: once the old target is gone, the completed copy is the
    // only one left, so a replacement that fails keeps it and names it instead of
    // throwing it away.
    if existing && tokio::fs::remove_file(&target).await.is_err() {
        return End::Failed(
            TransferFailure::FinalizeFailed,
            Some(temporary.display().to_string()),
        );
    }
    match tokio::fs::rename(&temporary, &target).await {
        Ok(()) => End::Completed(written),
        Err(_) => End::Failed(
            TransferFailure::FinalizeFailed,
            Some(temporary.display().to_string()),
        ),
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
        Decision::Keep | Decision::Cancel => Question::Stop(End::Cancelled(None)),
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
    }
}
