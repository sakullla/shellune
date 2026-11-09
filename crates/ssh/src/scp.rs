//! Copying files with the traditional SCP protocol (R10).
//!
//! The file view and its transfer queue copy files over SFTP. A server that offers no
//! SFTP subsystem still runs its own `scp` program, and this module is what copies a
//! file - or, when the user asked for it, a whole directory - through that program on
//! the connection the terminal already authenticated. It is the traditional protocol:
//! `scp -t` for an upload (the remote side is the sink) and `scp -f` for a download (the
//! remote side is the source), with control records and acknowledgements on the
//! channel's own byte stream.
//!
//! What this module may and may not do follows from that protocol:
//!
//! * it copies; it never lists, creates, renames or deletes anything except the tree it
//!   was asked to copy. Directory operations stay SFTP operations, and a window showing
//!   a failed SCP copy says so instead of drawing a listing;
//! * a path is data. Every argument is quoted for the remote shell before it becomes
//!   part of the command, so a space, a quote, a semicolon or a `$(...)` in a file name
//!   cannot turn into a second command;
//! * nothing is written before the answer about an existing target. Only after the user
//!   said to replace it does an upload start its `scp -t`, and only then can the target
//!   stop being what it was;
//! * there is no resume. The traditional protocol has no way to check a prefix, so a
//!   cancelled or failed task reports the file that holds partial content and offers no
//!   offset;
//! * a download accepts only the name it asked for. A server that answers with another
//!   file is refused, because a copy that silently lands somewhere else is worse than
//!   no copy at all.
//!
//! One compatibility fact is measured rather than assumed: OpenSSH 9 runs `scp` over
//! SFTP unless `-O` selects the traditional protocol, and OpenSSH 8 does not know `-O`
//! at all. Which of the two this server's program is gets asked once per task
//! ([`remote_program`]); both answers are the same user-visible protocol.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use russh::client;
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};
use shellune_core::{ResumeRefusal, TransferDirection, TransferFailure, parent_path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::session::Command;
use crate::sftp::ChannelFailure;
use crate::transfer::{End, Job, PROGRESS_INTERVAL, Question, TransferRequest, ask_about_target};

/// How much file content is moved between two checks for a cancellation.
const CHUNK: usize = 64 * 1024;

/// How long one read may wait for the remote side before the connection is treated as
/// gone. Long on purpose: it is a silence detector, not a deadline for the copy.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest message one control record may carry, so a server cannot make this
/// process allocate without a bound.
const MAX_MESSAGE: usize = 4096;

/// What the readiness probe prints when the account has no `scp` program.
const NO_PROGRAM: &str = "shellune-no-scp";

/// How one SCP exchange stopped, before the residue is decided.
#[derive(Debug)]
enum Stop {
    /// The user asked for the copy to stop.
    Cancelled,
    /// The copy failed for a reason the user can read.
    Failed(TransferFailure),
}

impl From<TransferFailure> for Stop {
    fn from(failure: TransferFailure) -> Self {
        Self::Failed(failure)
    }
}

/// One step of an SCP exchange.
type Step<T> = std::result::Result<T, Stop>;

impl Stop {
    /// The failure this stop is, if it is one.
    fn failure(&self) -> Option<&TransferFailure> {
        match self {
            Self::Failed(failure) => Some(failure),
            Self::Cancelled => None,
        }
    }
}

/// The channel of one remote `scp`, read record by record.
///
/// The socket delivers chunks that have nothing to do with record boundaries, so the
/// bytes that arrived but were not consumed yet are kept here. Errors and exit status
/// are collected beside them: they are what turns "the copy stopped" into a reason the
/// user can act on.
struct Pipe {
    read: ChannelReadHalf,
    write: ChannelWriteHalf<client::Msg>,
    /// Bytes that arrived and were not consumed yet.
    pending: Vec<u8>,
    /// How many of [`Self::pending`] were already consumed.
    taken: usize,
    /// What the remote program wrote to its error stream.
    stderr: String,
    /// The status the remote program exited with, when it said.
    exit: Option<u32>,
    /// The channel ended: nothing more will arrive.
    over: bool,
    /// The server refused to run the command at all.
    refused: bool,
}

impl Pipe {
    fn new(read: ChannelReadHalf, write: ChannelWriteHalf<client::Msg>) -> Self {
        Self {
            read,
            write,
            pending: Vec::new(),
            taken: 0,
            stderr: String::new(),
            exit: None,
            over: false,
            refused: false,
        }
    }

    /// Takes in one message from the channel.
    fn absorb(&mut self, message: Option<ChannelMsg>) {
        match message {
            Some(ChannelMsg::Data { data }) => {
                if self.taken >= self.pending.len() {
                    self.pending.clear();
                    self.taken = 0;
                    self.pending.extend_from_slice(&data);
                } else {
                    self.pending.extend_from_slice(&data);
                }
            }
            Some(ChannelMsg::ExtendedData { data, ext }) => {
                // Extension 1 is stderr, where the remote scp writes why it stopped.
                if ext == 1 {
                    self.note_stderr(&String::from_utf8_lossy(&data));
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => self.exit = Some(exit_status),
            Some(ChannelMsg::ExitSignal {
                error_message,
                signal_name,
                ..
            }) => {
                self.note_stderr(&format!("{signal_name:?} {error_message}"));
            }
            // A success reply to the exec request says the program started; a failure
            // says the server would not run it, which is its own reason.
            Some(ChannelMsg::Failure) => self.refused = true,
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => self.over = true,
            Some(_) => {}
        }
    }

    /// Keeps a bounded tail of what the remote program complained about.
    fn note_stderr(&mut self, text: &str) {
        self.stderr.push_str(text);
        if self.stderr.len() > MAX_MESSAGE {
            let cut = self.stderr.len() - MAX_MESSAGE;
            // Cut on a character boundary: a half character would be unreadable.
            let cut = self
                .stderr
                .char_indices()
                .map(|(index, _)| index)
                .find(|index| *index >= cut)
                .unwrap_or(self.stderr.len());
            self.stderr.drain(..cut);
        }
    }

    /// Waits for the next message when nothing is buffered, and reports the end.
    async fn next_message(&mut self) -> Step<()> {
        loop {
            if self.taken < self.pending.len() {
                return Ok(());
            }
            self.pending.clear();
            self.taken = 0;
            if self.over {
                return Ok(());
            }
            let message = tokio::time::timeout(IDLE_TIMEOUT, self.read.wait())
                .await
                .map_err(|_| Stop::Failed(TransferFailure::ConnectionLost))?;
            self.absorb(message);
            if self.refused {
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: "the server refused to run its scp program".to_owned(),
                }));
            }
        }
    }

    /// The next byte, or `None` when the remote side ended the channel.
    async fn read_byte(&mut self) -> Step<Option<u8>> {
        self.next_message().await?;
        if self.taken >= self.pending.len() {
            return Ok(None);
        }
        let byte = self.pending[self.taken];
        self.taken += 1;
        Ok(Some(byte))
    }

    /// Up to `want` bytes from the stream, and how many were returned.
    ///
    /// Zero means the channel ended. File content is read in pieces rather than whole,
    /// so a big file never has to fit in memory.
    async fn read_part(&mut self, want: usize, buffer: &mut Vec<u8>) -> Step<usize> {
        let Some(first) = self.read_byte().await? else {
            return Ok(0);
        };
        buffer.clear();
        buffer.push(first);
        let available = self.pending.len() - self.taken;
        let extra = want.saturating_sub(1).min(available);
        buffer.extend_from_slice(&self.pending[self.taken..self.taken + extra]);
        self.taken += extra;
        Ok(buffer.len())
    }

    /// One line of a control record, without its newline.
    ///
    /// The bytes are collected first and decoded as text at the end: a name with
    /// Chinese characters or an emoji is several bytes per character, and reading them
    /// one at a time as characters would turn each of them into something else.
    async fn read_line(&mut self) -> Step<String> {
        let mut bytes = Vec::new();
        loop {
            let Some(byte) = self.read_byte().await? else {
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: "the remote copy program ended in the middle of a record".to_owned(),
                }));
            };
            if byte == b'\n' {
                return String::from_utf8(bytes).map_err(|_| {
                    Stop::Failed(TransferFailure::Refused {
                        reason: "the remote copy program sent a name that is not valid text"
                            .to_owned(),
                    })
                });
            }
            if bytes.len() >= MAX_MESSAGE {
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: "the remote copy program sent a record that is too long".to_owned(),
                }));
            }
            bytes.push(byte);
        }
    }

    /// One acknowledgement, which is a zero byte.
    async fn read_ack(&mut self, waiting_for: &str) -> Step<()> {
        match self.read_byte().await? {
            Some(0) => Ok(()),
            Some(1) | Some(2) => Err(self.message_failure().await),
            Some(other) => Err(Stop::Failed(TransferFailure::Refused {
                reason: format!(
                    "the remote copy program sent 0x{other:02x} while {waiting_for} was expected"
                ),
            })),
            None => Err(self.ended_failure()),
        }
    }

    /// The reason a control record carried, as the remote program wrote it.
    async fn message_failure(&mut self) -> Stop {
        match self.read_line().await {
            Ok(line) if !line.trim().is_empty() => Stop::Failed(classify(&line)),
            _ => self.ended_failure(),
        }
    }

    /// The failure to report when the channel ended where an answer was required.
    fn ended_failure(&self) -> Stop {
        let text = self.stderr_text();
        if text.is_empty() {
            Stop::Failed(TransferFailure::Refused {
                reason: "the remote copy program ended before it answered".to_owned(),
            })
        } else {
            Stop::Failed(classify(&text))
        }
    }

    /// What the remote program wrote to its error stream, as one line.
    fn stderr_text(&self) -> String {
        let text = self
            .stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        text.chars().take(400).collect()
    }

    /// Sends bytes to the remote program.
    async fn write_all(&mut self, data: &[u8]) -> Step<()> {
        self.write
            .data_bytes(data.to_vec())
            .await
            .map_err(|_| Stop::Failed(TransferFailure::ConnectionLost))
    }

    /// A number in a control record, which the protocol writes in decimal.
    async fn write_record(&mut self, record: &str) -> Step<()> {
        self.write_all(record.as_bytes()).await
    }

    /// Ends the channel and takes in whatever the remote program still says.
    ///
    /// The exit status and the error text are read after the last record, so a program
    /// that failed at the very end is reported with its own words instead of a generic
    /// "the copy stopped".
    async fn settle(&mut self) {
        let _ = self.write.eof().await;
        let _ = self.write.close().await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.over && Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), self.read.wait()).await {
                Ok(message) => {
                    let ended = message.is_none();
                    self.absorb(message);
                    if ended {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }

    /// Reports a remote program that exited with a failure after the last record.
    fn check_exit(&self) -> Step<()> {
        match self.exit {
            Some(0) | None => Ok(()),
            Some(code) => {
                let text = self.stderr_text();
                if text.is_empty() {
                    Err(Stop::Failed(TransferFailure::Refused {
                        reason: format!("the remote copy program exited with status {code}"),
                    }))
                } else {
                    Err(Stop::Failed(classify(&text)))
                }
            }
        }
    }
}

/// The failure a message from the remote program describes.
///
/// The words are the program's own; only the kind is decided here, so a full disk, a
/// missing path and a refused write stay different reasons instead of one "it failed".
fn classify(message: &str) -> TransferFailure {
    let text = message.to_lowercase();
    if text.contains("no space") || text.contains("quota") || text.contains("disk full") {
        TransferFailure::NoSpace
    } else if text.contains("permission denied") {
        TransferFailure::PermissionDenied
    } else if text.contains("no such file") || text.contains("not found") {
        TransferFailure::NotFound
    } else if text.contains("connection") || text.contains("broken pipe") {
        TransferFailure::ConnectionLost
    } else {
        TransferFailure::Refused {
            reason: message.trim().to_owned(),
        }
    }
}

/// How this server's `scp` is asked to speak the traditional protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteProgram {
    /// The program knows `-O`, which is what selects the traditional protocol there.
    Explicit,
    /// The program predates `-O` and speaks the traditional protocol by default.
    Implicit,
}

impl RemoteProgram {
    /// The option that makes this program speak the traditional protocol.
    fn option(self) -> &'static str {
        match self {
            Self::Explicit => "-O ",
            Self::Implicit => "",
        }
    }
}

/// One entry of a copy, in the order the records are sent.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    /// One file and what it holds.
    File {
        path: PathBuf,
        name: String,
        size: u64,
    },
    /// One directory and its children.
    Directory { name: String, children: Vec<Node> },
}

impl Node {
    /// The bytes this entry holds, which is the total an upload can report.
    fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Directory { children, .. } => children.iter().map(Self::size).sum(),
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory { .. })
    }
}

/// Why a local source could not be planned.
struct Unreadable;

impl From<Unreadable> for Stop {
    fn from(_: Unreadable) -> Self {
        Self::Failed(TransferFailure::SourceUnreadable)
    }
}

/// Copies one local file or directory to a remote path.
pub(crate) async fn upload(
    commands: &UnboundedSender<Command>,
    request: &TransferRequest,
    job: &mut Job,
) -> End {
    let mut written = 0u64;
    let mut started = false;
    let result = upload_inner(commands, request, job, &mut written, &mut started).await;
    // A traditional SCP upload writes into the target's own name: the protocol has no
    // rename, so a stopped copy leaves part of the content exactly there.
    let residue = started.then(|| request.destination.clone());
    match result {
        Ok(()) => End::Completed(written),
        Err(Stop::Cancelled) => End::Cancelled(residue, None),
        Err(stop) => End::Failed(
            stop.failure()
                .cloned()
                .unwrap_or(TransferFailure::ConnectionLost),
            residue,
            None,
        ),
    }
}

async fn upload_inner(
    commands: &UnboundedSender<Command>,
    request: &TransferRequest,
    job: &mut Job,
    written: &mut u64,
    started: &mut bool,
) -> Step<()> {
    let local = PathBuf::from(&request.source);
    let metadata = tokio::fs::metadata(&local)
        .await
        .map_err(|_| Stop::Failed(TransferFailure::SourceUnreadable))?;
    let Some(name) = file_name(&request.destination) else {
        return Err(Stop::Failed(TransferFailure::Refused {
            reason: "the destination names no file".to_owned(),
        }));
    };
    if metadata.is_dir() && !request.recursive {
        return Err(Stop::Failed(TransferFailure::Refused {
            reason: "the source is a directory; copying it needs an explicit recursive copy"
                .to_owned(),
        }));
    }
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(Stop::Failed(TransferFailure::SourceUnreadable));
    }

    let plan = if metadata.is_dir() {
        let root = local.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || plan_directory(&root, name))
            .await
            .map_err(|_| Stop::Failed(TransferFailure::SourceUnreadable))??
    } else {
        Node::File {
            path: local,
            name: name.to_owned(),
            size: metadata.len(),
        }
    };
    let total = plan.size();

    // Nothing is written until the question about an existing target is answered, so a
    // "keep the existing one" and a cancellation both leave the target as it was.
    let existing = remote_exists(commands, &request.destination).await?;
    match ask_about_target(existing, Some(total), job).await {
        Question::Replace => {}
        // The answer wrote nothing: keeping the target is the copy ending without it.
        Question::Stop(_) => return Err(Stop::Cancelled),
    }

    let program = remote_program(commands).await?;
    let target_directory = parent_path(&request.destination).unwrap_or_else(|| ".".to_owned());
    let mut options = String::new();
    if plan.is_directory() {
        options.push_str("-r ");
    }
    options.push_str(program.option());
    options.push_str("-d -t ");
    let command = format!("scp {options}-- {}", shell_argument(&target_directory));

    let mut pipe = start(commands, command).await.map_err(Stop::Failed)?;
    // The remote sink says it is ready before anything is sent into it. Nothing exists
    // on the other side yet, so this is not a residue.
    pipe.read_ack("the copy started").await?;

    let mut work = vec![Send::Node(&plan)];
    let mut last_report = Instant::now();
    let mut buffer = vec![0u8; CHUNK];
    while let Some(task) = work.pop() {
        if job.interrupted() {
            pipe.settle().await;
            return Err(Stop::Cancelled);
        }
        match task {
            Send::Node(Node::Directory { name, children }) => {
                pipe.write_record(&format!("D0755 0 {name}\n")).await?;
                pipe.read_ack("a directory was announced").await?;
                // The server has now created something under the target's name, so a
                // stop from here on leaves part of the tree there.
                *started = true;
                // The matching `E` closes this directory after its children.
                work.push(Send::Close);
                for child in children.iter().rev() {
                    work.push(Send::Node(child));
                }
            }
            Send::Close => {
                pipe.write_record("E\n").await?;
                pipe.read_ack("a directory was closed").await?;
            }
            Send::Node(Node::File { path, name, size }) => {
                reject_unrepresentable(name)?;
                pipe.write_record(&format!("C0644 {size} {name}\n")).await?;
                pipe.read_ack("a file was announced").await?;
                // The target's name now exists, so a stop from here on leaves part of
                // the content in it.
                *started = true;
                let mut file = tokio::fs::File::open(path)
                    .await
                    .map_err(|_| Stop::Failed(TransferFailure::SourceUnreadable))?;
                let mut sent = 0u64;
                loop {
                    if job.interrupted() {
                        pipe.settle().await;
                        return Err(Stop::Cancelled);
                    }
                    let read = file
                        .read(&mut buffer)
                        .await
                        .map_err(|_| Stop::Failed(TransferFailure::SourceUnreadable))?;
                    if read == 0 {
                        break;
                    }
                    sent += read as u64;
                    if sent > *size {
                        return Err(Stop::Failed(TransferFailure::Refused {
                            reason: "the file grew while it was being copied".to_owned(),
                        }));
                    }
                    pipe.write_all(&buffer[..read]).await?;
                    *written += read as u64;
                    if last_report.elapsed() >= PROGRESS_INTERVAL {
                        job.progress(*written, Some(total));
                        last_report = Instant::now();
                    }
                }
                if sent != *size {
                    return Err(Stop::Failed(TransferFailure::Refused {
                        reason: "the file changed size while it was being copied".to_owned(),
                    }));
                }
                // The trailing zero byte is the end-of-file marker the sink waits for.
                pipe.write_all(&[0]).await?;
                pipe.read_ack("a file ended").await?;
            }
        }
    }
    job.progress(*written, Some(total));
    pipe.settle().await;
    pipe.check_exit()?;
    Ok(())
}

/// One unit of work of an upload, so a tree is sent without recursion.
enum Send<'a> {
    Node(&'a Node),
    Close,
}

/// Copies one remote file or directory into a local path.
pub(crate) async fn download(
    commands: &UnboundedSender<Command>,
    request: &TransferRequest,
    job: &mut Job,
) -> End {
    let partial = PathBuf::from(shellune_core::partial_path(
        TransferDirection::Download,
        &request.destination,
        request.transfer,
    ));
    let mut written = 0u64;
    let result = download_inner(commands, request, job, &partial, &mut written).await;
    match result {
        Ok(()) => End::Completed(written),
        Err(Stop::Cancelled) => End::Cancelled(discard(&partial).await, None),
        Err(stop) => End::Failed(
            stop.failure()
                .cloned()
                .unwrap_or(TransferFailure::ConnectionLost),
            discard(&partial).await,
            None,
        ),
    }
}

async fn download_inner(
    commands: &UnboundedSender<Command>,
    request: &TransferRequest,
    job: &mut Job,
    partial: &Path,
    written: &mut u64,
) -> Step<()> {
    let Some(name) = file_name(&request.source) else {
        return Err(Stop::Failed(TransferFailure::Refused {
            reason: "the source names no file".to_owned(),
        }));
    };
    let source_kind = remote_kind(commands, &request.source).await?;
    let directory = match source_kind {
        RemoteKind::Missing => return Err(Stop::Failed(TransferFailure::NotFound)),
        RemoteKind::Directory if !request.recursive => {
            return Err(Stop::Failed(TransferFailure::Refused {
                reason: "the source is a directory; copying it needs an explicit recursive copy"
                    .to_owned(),
            }));
        }
        RemoteKind::Directory => true,
        RemoteKind::File => false,
    };

    let target = PathBuf::from(&request.destination);
    let existing = tokio::fs::symlink_metadata(&target).await.is_ok();
    match ask_about_target(existing, None, job).await {
        Question::Replace => {}
        Question::Stop(_) => return Err(Stop::Cancelled),
    }

    let program = remote_program(commands).await?;
    let mut options = String::new();
    if directory {
        options.push_str("-r ");
    }
    options.push_str(program.option());
    options.push_str("-f ");
    let command = format!("scp {options}-- {}", shell_argument(&request.source));
    let mut pipe = start(commands, command).await.map_err(Stop::Failed)?;
    // The source waits for an acknowledgement before it describes the first entry.
    pipe.write_all(&[0]).await?;

    let record = next_record(&mut pipe).await?;
    match record {
        Record::File { size, ref sent } if !directory => {
            // Only the name that was asked for is accepted: a copy that lands under
            // another name than the one requested has to be refused, not written.
            if sent != name {
                pipe.settle().await;
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: format!("the server sent {sent} instead of the requested file {name}"),
                }));
            }
            pipe.write_all(&[0]).await?;
            let mut file = tokio::fs::File::create(partial)
                .await
                .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
            receive_file(&mut pipe, job, &mut file, size, size, written).await?;
            file.flush()
                .await
                .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
        }
        Record::Directory { ref sent } if directory => {
            if sent != name {
                pipe.settle().await;
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: format!(
                        "the server sent {sent} instead of the requested directory {name}"
                    ),
                }));
            }
            tokio::fs::create_dir_all(partial)
                .await
                .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
            pipe.write_all(&[0]).await?;
            receive_tree(&mut pipe, job, partial, written).await?;
        }
        Record::Done => {
            return Err(Stop::Failed(TransferFailure::Refused {
                reason: "the remote copy program sent nothing".to_owned(),
            }));
        }
        _ => {
            pipe.settle().await;
            return Err(Stop::Failed(TransferFailure::Refused {
                reason: format!("the server described {name} as something else"),
            }));
        }
    }
    pipe.settle().await;
    pipe.check_exit()?;

    // Only now is the target touched: the whole copy is in the temporary path.
    if existing && remove_local(&target).await.is_err() {
        return Err(Stop::Failed(TransferFailure::FinalizeFailed));
    }
    tokio::fs::rename(partial, &target)
        .await
        .map_err(|_| Stop::Failed(TransferFailure::FinalizeFailed))?;
    Ok(())
}

/// One control record the remote source sent.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Record {
    /// One file of `size` bytes under `sent`.
    File { size: u64, sent: String },
    /// One directory named `sent`.
    Directory { sent: String },
    /// The end of the directory being received.
    End,
    /// The channel ended: everything that was asked for arrived.
    Done,
}

/// The next control record, with warnings and timestamps handled where they appear.
async fn next_record(pipe: &mut Pipe) -> Step<Record> {
    loop {
        match pipe.read_byte().await? {
            None => return Ok(Record::Done),
            // A zero byte is an acknowledgement from the source; nothing to do.
            Some(0) => continue,
            Some(1) | Some(2) => return Err(pipe.message_failure().await),
            Some(b'C') => {
                let line = pipe.read_line().await?;
                let (_, size, sent) = parse_record(&line)?;
                return Ok(Record::File { size, sent });
            }
            Some(b'D') => {
                let line = pipe.read_line().await?;
                let (_, _, sent) = parse_record(&line)?;
                return Ok(Record::Directory { sent });
            }
            // The end of a directory is its own line: `E` and then a newline.
            Some(b'E') => {
                let rest = pipe.read_line().await?;
                if !rest.trim().is_empty() {
                    return Err(Stop::Failed(TransferFailure::Refused {
                        reason: "the remote copy program sent an unreadable directory end"
                            .to_owned(),
                    }));
                }
                return Ok(Record::End);
            }
            // A timestamp record: read it and acknowledge it, as the protocol expects.
            Some(b'T') => {
                let _ = pipe.read_line().await?;
                pipe.write_all(&[0]).await?;
            }
            Some(other) => {
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: format!(
                        "the remote copy program sent an unexpected record 0x{other:02x}"
                    ),
                }));
            }
        }
    }
}

/// Receives the entries of one directory, in the order the records arrive.
async fn receive_tree(pipe: &mut Pipe, job: &mut Job, root: &Path, written: &mut u64) -> Step<()> {
    let mut stack = vec![root.to_path_buf()];
    loop {
        if job.interrupted() {
            pipe.settle().await;
            return Err(Stop::Cancelled);
        }
        let record = next_record(pipe).await?;
        let current = stack.last().cloned().unwrap_or_else(|| root.to_path_buf());
        match record {
            Record::Done => return Ok(()),
            Record::File { size, sent } => {
                confined(&sent)?;
                pipe.write_all(&[0]).await?;
                let path = current.join(&sent);
                let mut file = tokio::fs::File::create(&path)
                    .await
                    .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
                receive_file(pipe, job, &mut file, size, size, written).await?;
                file.flush()
                    .await
                    .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
            }
            Record::Directory { sent } => {
                confined(&sent)?;
                let path = current.join(&sent);
                tokio::fs::create_dir_all(&path)
                    .await
                    .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
                pipe.write_all(&[0]).await?;
                stack.push(path);
            }
            Record::End => {
                pipe.write_all(&[0]).await?;
                stack.pop();
                if stack.is_empty() {
                    return Ok(());
                }
            }
        }
    }
}

/// Reads one file's content into a local file, then its end marker.
async fn receive_file(
    pipe: &mut Pipe,
    job: &mut Job,
    file: &mut tokio::fs::File,
    size: u64,
    total: u64,
    written: &mut u64,
) -> Step<()> {
    let mut left = size;
    let mut buffer = Vec::new();
    let mut last_report = Instant::now();
    while left > 0 {
        if job.interrupted() {
            pipe.settle().await;
            return Err(Stop::Cancelled);
        }
        let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
        let read = pipe.read_part(want, &mut buffer).await?;
        if read == 0 {
            return Err(pipe.ended_failure());
        }
        file.write_all(&buffer[..read])
            .await
            .map_err(|_| Stop::Failed(TransferFailure::DestinationUnwritable))?;
        left -= read as u64;
        *written += read as u64;
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            job.progress(*written, Some(total));
            last_report = Instant::now();
        }
    }
    job.progress(*written, Some(total));
    // The source writes one zero byte after the content; the sink acknowledges it.
    match pipe.read_byte().await? {
        Some(0) => {}
        Some(1) | Some(2) => return Err(pipe.message_failure().await),
        Some(other) => {
            return Err(Stop::Failed(TransferFailure::Refused {
                reason: format!("the remote copy program sent 0x{other:02x} after the content"),
            }));
        }
        None => return Err(pipe.ended_failure()),
    }
    pipe.write_all(&[0]).await?;
    Ok(())
}

/// Reads `C<mode> <size> <name>` or `D<mode> <size> <name>` after its first letter.
fn parse_record(line: &str) -> Step<(u32, u64, String)> {
    let mut parts = line.splitn(3, ' ');
    let mode = parts.next().unwrap_or_default();
    let size = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let mode = u32::from_str_radix(mode.trim(), 8).map_err(|_| {
        Stop::Failed(TransferFailure::Refused {
            reason: "the remote copy program sent an unreadable record".to_owned(),
        })
    })?;
    let size = size.trim().parse::<u64>().map_err(|_| {
        Stop::Failed(TransferFailure::Refused {
            reason: "the remote copy program sent an unreadable size".to_owned(),
        })
    })?;
    if name.is_empty() {
        return Err(Stop::Failed(TransferFailure::Refused {
            reason: "the remote copy program sent an entry without a name".to_owned(),
        }));
    }
    Ok((mode, size, name.to_owned()))
}

/// Refuses a name that would not survive the protocol intact.
///
/// The record carries the name as one line, so a line break would end it early and the
/// rest of the name would be read as the next record.
fn reject_unrepresentable(name: &str) -> Step<()> {
    if name.is_empty() || name.contains(['\n', '\r', '\0']) {
        return Err(Stop::Failed(TransferFailure::Refused {
            reason: format!("{name:?} cannot be copied: the protocol carries no such name"),
        }));
    }
    Ok(())
}

/// Refuses a received name that is not one entry of the directory being written.
///
/// A server naming `../x`, an absolute path or a name with a separator would write
/// outside the directory the user chose, so such a record stops the copy.
fn confined(name: &str) -> Step<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\n', '\r', '\0'])
    {
        return Err(Stop::Failed(TransferFailure::Refused {
            reason: format!("the server sent the entry name {name:?}, which is not one name"),
        }));
    }
    Ok(())
}

/// The last name of a path, when the path names something.
fn file_name(path: &str) -> Option<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.ends_with('/') {
        return None;
    }
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name)
}

/// One shell word that is only ever the path it was built from.
///
/// The remote program is started by the server's shell, so the path has to be quoted
/// before it becomes part of the command: a space, a quote, a semicolon or a `$(...)`
/// in a file name must stay one name. A leading `~/` is the one thing that is expanded,
/// because that is what a typed `~/file` means.
fn shell_argument(path: &str) -> String {
    if path == "~" {
        return "\"$HOME\"".to_owned();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("\"$HOME\"/{}", single_quoted(rest));
    }
    single_quoted(path)
}

/// One argument in single quotes, with quotes inside it escaped for the shell.
fn single_quoted(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for character in text.chars() {
        if character == '\'' {
            // End the quoted word, add an escaped quote, start the word again.
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

/// Starts one remote `scp` on the connection that is already authenticated.
async fn start(
    commands: &UnboundedSender<Command>,
    command: String,
) -> std::result::Result<Pipe, TransferFailure> {
    let (reply, answer) = oneshot::channel();
    if commands.send(Command::Exec { command, reply }).is_err() {
        return Err(TransferFailure::ConnectionLost);
    }
    match answer.await {
        Ok(Ok(channel)) => {
            let (read, write) = channel.split();
            Ok(Pipe::new(read, write))
        }
        Ok(Err(ChannelFailure::Unavailable { reason })) => Err(TransferFailure::Refused { reason }),
        Ok(Err(ChannelFailure::Lost { .. })) | Err(_) => Err(TransferFailure::ConnectionLost),
    }
}

/// Runs one command and returns everything it printed, for the two small questions this
/// module has to ask before it can copy anything.
async fn ask_remote(
    commands: &UnboundedSender<Command>,
    command: String,
) -> std::result::Result<String, TransferFailure> {
    let mut pipe = start(commands, command).await?;
    let mut output = Vec::new();
    let mut buffer = Vec::new();
    loop {
        let read = pipe.read_part(CHUNK, &mut buffer).await.map_err(|stop| {
            stop.failure()
                .cloned()
                .unwrap_or(TransferFailure::ConnectionLost)
        })?;
        if read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..read]);
        if output.len() > MAX_MESSAGE {
            break;
        }
    }
    pipe.settle().await;
    pipe.check_exit().map_err(|stop| {
        stop.failure()
            .cloned()
            .unwrap_or(TransferFailure::ConnectionLost)
    })?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Whether the remote account's `scp` knows the option that selects the traditional protocol.
///
/// OpenSSH 9 runs `scp` over SFTP unless `-O` says otherwise, and OpenSSH 8 has no `-O`
/// at all. The question is asked once per task, on the same authenticated connection,
/// and both answers mean the same thing to the user: the traditional SCP protocol.
async fn remote_program(
    commands: &UnboundedSender<Command>,
) -> std::result::Result<RemoteProgram, TransferFailure> {
    let command = format!(
        "if command -v scp >/dev/null 2>&1; then scp -O 2>&1 | head -n 60; else printf '{NO_PROGRAM}'; fi"
    );
    let output = ask_remote(commands, command).await?;
    if output.contains(NO_PROGRAM) {
        return Err(TransferFailure::Refused {
            reason: "the server has no scp program, so no SCP copy can run here".to_owned(),
        });
    }
    let text = output.to_lowercase();
    let older = [
        "unknown option",
        "illegal option",
        "invalid option",
        "unrecognized option",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    Ok(if older {
        RemoteProgram::Implicit
    } else {
        RemoteProgram::Explicit
    })
}

/// Whether a remote path exists, asked without SFTP.
async fn remote_exists(commands: &UnboundedSender<Command>, path: &str) -> Step<bool> {
    let argument = shell_argument(path);
    let command =
        format!("if [ -e {argument} ] || [ -L {argument} ]; then printf yes; else printf no; fi");
    let output = ask_remote(commands, command).await.map_err(Stop::Failed)?;
    match output.trim() {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(Stop::Failed(TransferFailure::Refused {
            reason: "the server could not say whether the target exists".to_owned(),
        })),
    }
}

/// What a remote source is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteKind {
    Missing,
    File,
    Directory,
}

/// Whether a remote source is a file, a directory or nothing, asked without SFTP.
async fn remote_kind(commands: &UnboundedSender<Command>, path: &str) -> Step<RemoteKind> {
    let argument = shell_argument(path);
    let command = format!(
        "if [ -d {argument} ]; then printf directory; elif [ -e {argument} ]; then printf file; else printf missing; fi"
    );
    let output = ask_remote(commands, command).await.map_err(Stop::Failed)?;
    match output.trim() {
        "directory" => Ok(RemoteKind::Directory),
        "file" => Ok(RemoteKind::File),
        "missing" => Ok(RemoteKind::Missing),
        _ => Err(Stop::Failed(TransferFailure::Refused {
            reason: "the server could not describe the source".to_owned(),
        })),
    }
}

/// The whole subtree of a directory, in the order it will be sent.
///
/// The walk is metadata-only and runs off the async runtime: an upload has to know the
/// total size before its first record, and a tree cannot be walked without reading its
/// directories.
fn plan_directory(root: &Path, name: String) -> std::result::Result<Node, Stop> {
    let mut children = Vec::new();
    let read = std::fs::read_dir(root).map_err(|_| Unreadable)?;
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|_| Unreadable)?;
        entries.push(entry.path());
    }
    // Sorted, so the same tree is sent in the same order every time.
    entries.sort();
    for path in entries {
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| Unreadable)?;
        let Some(child_name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        // A name the protocol cannot carry is refused before anything is sent, so the
        // copy does not stop halfway through a tree.
        reject_unrepresentable(&child_name)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            // A link to a file is copied as the content it points at; a link to a
            // directory is refused instead of followed, because following it could copy
            // a tree the user did not select - or loop forever.
            let target = std::fs::metadata(&path).map_err(|_| Unreadable)?;
            if target.is_dir() {
                return Err(Stop::Failed(TransferFailure::Refused {
                    reason: format!(
                        "{} is a link to a directory, which a recursive copy does not follow",
                        path.display()
                    ),
                }));
            }
            children.push(Node::File {
                path: path.clone(),
                name: child_name,
                size: target.len(),
            });
        } else if file_type.is_dir() {
            children.push(plan_directory(&path, child_name)?);
        } else if file_type.is_file() {
            children.push(Node::File {
                path: path.clone(),
                name: child_name,
                size: metadata.len(),
            });
        } else {
            // A socket, a device or a fifo is not content that can be copied as a file.
            return Err(Stop::Failed(TransferFailure::Refused {
                reason: format!("{} is not a file or a directory", path.display()),
            }));
        }
    }
    Ok(Node::Directory { name, children })
}

/// Removes a temporary path, naming it when it could not be removed.
async fn discard(path: &Path) -> Option<String> {
    if tokio::fs::symlink_metadata(path).await.is_err() {
        return None;
    }
    if remove_local(path).await.is_ok() {
        None
    } else {
        Some(path.display().to_string())
    }
}

/// Removes a local file or a whole local directory tree.
async fn remove_local(path: &Path) -> std::io::Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    }
}

/// A resume that SCP cannot provide, kept here so the two directions answer the same.
///
/// It exists only to make the reason explicit: a checkpoint that reached an SCP task
/// (from an older record, or from a task whose protocol was changed) is refused before a
/// byte is written instead of being ignored, which is what "SCP does not resume" means
/// to the person who asked for it.
#[must_use]
pub(crate) fn refuse_resume(request: &TransferRequest) -> Option<TransferFailure> {
    request
        .resume
        .as_ref()
        .map(|_| TransferFailure::ResumeRefused {
            reason: ResumeRefusal::Unsupported,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_one_shell_word_whatever_it_contains() {
        assert_eq!(
            single_quoted("/home/deploy/report.txt"),
            "'/home/deploy/report.txt'"
        );
        // A space stays inside the word, so the remote program sees one argument.
        assert_eq!(
            shell_argument("/home/deploy/报告 终稿.txt"),
            "'/home/deploy/报告 终稿.txt'"
        );
        // A quote cannot end the word early.
        assert_eq!(
            shell_argument("/home/deploy/it's.txt"),
            "'/home/deploy/it'\\''s.txt'"
        );
        // A semicolon, a backtick and a command substitution stay data.
        for dangerous in ["; rm -rf /", "$(id)", "`id`", "a && b", "a | b"] {
            let quoted = shell_argument(&format!("/tmp/{dangerous}"));
            assert!(quoted.starts_with('\''), "{quoted}");
            assert!(quoted.ends_with('\''), "{quoted}");
            assert!(
                !quoted.contains(&format!(" {dangerous}")) || dangerous.starts_with(';'),
                "the whole path stays inside the quotes: {quoted}"
            );
        }
        assert_eq!(shell_argument("~/notes.txt"), "\"$HOME\"/'notes.txt'");
        assert_eq!(shell_argument("~"), "\"$HOME\"");
    }

    #[test]
    fn a_record_is_read_as_a_mode_a_size_and_a_name() {
        assert_eq!(
            parse_record("0644 1234 报告 终稿.txt").ok(),
            Some((0o644, 1234, "报告 终稿.txt".to_owned()))
        );
        assert_eq!(
            parse_record("0755 0 docs").ok(),
            Some((0o755, 0, "docs".to_owned()))
        );
        assert!(parse_record("0644 notanumber a.txt").is_err());
        assert!(parse_record("notoctal 12 a.txt").is_err());
        assert!(parse_record("0644 12 ").is_err(), "a record needs a name");
    }

    #[test]
    fn a_name_that_would_break_the_record_is_refused() {
        assert!(reject_unrepresentable("report.txt").is_ok());
        assert!(reject_unrepresentable("报告 终稿.txt").is_ok());
        assert!(reject_unrepresentable("").is_err());
        assert!(
            reject_unrepresentable("a\nb").is_err(),
            "a line break would end the record early"
        );
        assert!(reject_unrepresentable("a\rb").is_err());
    }

    #[test]
    fn a_received_name_must_be_one_entry_of_the_directory() {
        assert!(confined("report.txt").is_ok());
        assert!(confined("..").is_err());
        assert!(confined(".").is_err());
        assert!(confined("../escape.txt").is_err());
        assert!(confined("/etc/passwd").is_err());
        assert!(confined("dir/inside.txt").is_err());
        assert!(confined("dir\\inside.txt").is_err());
        assert!(confined("").is_err());
    }

    #[test]
    fn a_remote_message_becomes_the_failure_it_describes() {
        assert_eq!(
            classify("scp: /x: Permission denied"),
            TransferFailure::PermissionDenied
        );
        assert_eq!(
            classify("scp: /x: No such file or directory"),
            TransferFailure::NotFound
        );
        assert_eq!(
            classify("write failed: No space left on device"),
            TransferFailure::NoSpace
        );
        assert!(matches!(
            classify("scp: something else"),
            TransferFailure::Refused { .. }
        ));
        assert!(
            classify("Broken pipe").is_connection_lost(),
            "a broken pipe is the connection, not the server's refusal"
        );
    }

    #[test]
    fn a_planned_tree_counts_the_bytes_it_holds() {
        let root = std::env::temp_dir().join(format!("shellune-scp-plan-{}", std::process::id()));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("the directory can be made");
        std::fs::write(root.join("a.txt"), b"12345").expect("the file can be written");
        std::fs::write(nested.join("b.txt"), b"1234567").expect("the file can be written");

        let plan = plan_directory(&root, "root".to_owned()).expect("the tree is planned");
        assert_eq!(plan.size(), 12);
        let Node::Directory { name, children } = &plan else {
            panic!("the root is a directory");
        };
        assert_eq!(name, "root");
        assert_eq!(
            children,
            &vec![
                Node::File {
                    path: root.join("a.txt"),
                    name: "a.txt".to_owned(),
                    size: 5,
                },
                Node::Directory {
                    name: "nested".to_owned(),
                    children: vec![Node::File {
                        path: nested.join("b.txt"),
                        name: "b.txt".to_owned(),
                        size: 7,
                    }],
                },
            ],
            "entries are sent in a stable order, files and directories alike"
        );
        std::fs::remove_dir_all(&root).expect("the tree can be removed");
    }

    #[test]
    fn a_partial_path_is_the_one_the_task_derives_for_itself() {
        // The download side renames its temporary path onto the target, so the name has
        // to be the one a task without a worker can be told about.
        let id = shellune_core::TransferId::new();
        let path =
            shellune_core::partial_path(TransferDirection::Download, "C:/work/report.txt", id);
        assert!(path.ends_with(&format!("{}{id}", shellune_core::PARTIAL_PREFIX)));
    }
}
