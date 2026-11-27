//! Writing a terminal's output to a file while the session runs (R3).
//!
//! A [`SessionLog`] takes the bytes a terminal receives, exactly as they arrive, and
//! writes them on a thread of its own, so a slow disk never holds up the terminal.
//! Two formats:
//!
//! * [`LogFormat::Raw`] keeps every byte, escape sequences included: replaying the
//!   file through a terminal shows what the session showed.
//! * [`LogFormat::Plain`] keeps the readable text only. Escape sequences are parsed
//!   (across chunk boundaries, with the same parser the grid uses) and dropped, a
//!   line break is `\n`, and a lone carriage return is dropped. Full-screen
//!   programs redraw by moving the cursor, so their plain log reads poorly; the raw
//!   format is the one that keeps them.
//!
//! Only what the terminal received is written. Keys the user typed reach the file
//! only when the program echoed them back, the same as on screen.
//!
//! A write that fails stops the log: later bytes are not written, the failure is kept
//! for [`SessionLog::poll_error`], and the terminal goes on as before. The channel
//! to the writer is bounded; when it is full (the disk cannot keep up) bytes are
//! counted as dropped instead of making the terminal wait.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::vte::{Parser, Perform};

/// How the log keeps the output (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// The readable text, escape sequences removed.
    Plain,
    /// Every byte as the terminal received it.
    Raw,
}

/// How many chunks may wait for the writer before new ones are dropped.
const QUEUE: usize = 1024;

/// What a log did, reported when it stops.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogReport {
    /// Where it was written.
    pub path: PathBuf,
    /// Bytes of terminal output handed to the log and written (before any
    /// plain-text filtering).
    pub received: u64,
    /// Bytes of terminal output dropped because the writer fell behind.
    pub dropped: u64,
    /// Why writing stopped early, when it did.
    pub error: Option<String>,
}

enum Message {
    Bytes(Vec<u8>),
    Stop,
}

/// State shared between the terminal side and the writer thread.
#[derive(Default)]
struct Shared {
    received: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicBool,
    error: Mutex<Option<String>>,
}

/// One running log of a terminal's output (R3).
pub struct SessionLog {
    path: PathBuf,
    format: LogFormat,
    sender: Option<SyncSender<Message>>,
    writer: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    /// Whether [`SessionLog::poll_error`] already handed the failure out.
    reported: bool,
}

impl SessionLog {
    /// Creates (or truncates) the file at `path` and starts writing to it.
    ///
    /// # Errors
    ///
    /// Returns why the file could not be created; nothing is started then.
    pub fn start(path: &Path, format: LogFormat) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let (sender, receiver) = sync_channel(QUEUE);
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let target = Target::of(path, &file)?;
        let writer = std::thread::Builder::new()
            .name("session-log".to_owned())
            .spawn(move || write_loop(file, format, &target, &receiver, &thread_shared))?;
        Ok(Self {
            path: path.to_owned(),
            format,
            sender: Some(sender),
            writer: Some(writer),
            shared,
            reported: false,
        })
    }

    /// Where the log is written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The format it was started with.
    #[must_use]
    pub fn format(&self) -> LogFormat {
        self.format
    }

    /// Hands one chunk of terminal output to the writer without waiting.
    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() || self.shared.failed.load(Ordering::Acquire) {
            return;
        }
        let Some(sender) = &self.sender else {
            return;
        };
        let length = bytes.len() as u64;
        match sender.try_send(Message::Bytes(bytes.to_vec())) {
            Ok(()) => {
                self.shared.received.fetch_add(length, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.shared.dropped.fetch_add(length, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// The reason writing failed, the first time it is asked after the failure;
    /// `None` while the log is healthy and after the failure was handed out.
    pub fn poll_error(&mut self) -> Option<String> {
        if self.reported || !self.shared.failed.load(Ordering::Acquire) {
            return None;
        }
        self.reported = true;
        self.shared
            .error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    /// Writes what is queued, closes the file and says what happened.
    pub fn stop(mut self) -> LogReport {
        self.finish()
    }

    fn finish(&mut self) -> LogReport {
        if let Some(sender) = self.sender.take() {
            // A full queue still takes the stop once the writer has room: this waits
            // for the bytes already handed over, which is what "stop" promises.
            let _ = sender.send(Message::Stop);
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        LogReport {
            path: self.path.clone(),
            received: self.shared.received.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            error: self
                .shared
                .error
                .lock()
                .ok()
                .and_then(|error| error.clone()),
        }
    }
}

impl Drop for SessionLog {
    /// A log dropped with its terminal still writes what it was given.
    fn drop(&mut self) {
        self.finish();
    }
}

impl std::fmt::Debug for SessionLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLog")
            .field("path", &self.path)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// How often the writer checks that its file is still where, and what, it was.
pub const TARGET_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// The file a log writes to, as it was when the log started (R3).
///
/// Writing to an open file goes on succeeding after its folder is made read-only,
/// after it is deleted, or after another file takes its name: the operating system
/// checks access when a file is opened, not on each write. A log that went on like
/// that would be writing into nothing, so the writer checks the path itself every
/// [`TARGET_CHECK_INTERVAL`] while output arrives, and once more before it closes.
struct Target {
    path: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Target {
    fn of(path: &Path, file: &File) -> std::io::Result<Self> {
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let _ = file;
        Ok(Self {
            path: path.to_owned(),
            #[cfg(unix)]
            identity,
        })
    }

    /// Why the file can no longer be written as the user chose it, if it cannot.
    fn problem(&self) -> Option<String> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return Some(format!("{} no longer exists", self.path.display()));
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (metadata.dev(), metadata.ino()) != self.identity {
                return Some(format!(
                    "{} was replaced by another file",
                    self.path.display()
                ));
            }
        }
        // A character device such as /dev/null has no meaningful write bits to check.
        if !metadata.is_file() {
            return None;
        }
        if metadata.permissions().readonly() {
            return Some(format!("{} was made read-only", self.path.display()));
        }
        let folder = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        match std::fs::metadata(folder) {
            Ok(folder_metadata) if folder_metadata.permissions().readonly() => Some(format!(
                "the folder {} no longer accepts writes",
                folder.display()
            )),
            Ok(_) => None,
            Err(_) => Some(format!("the folder {} no longer exists", folder.display())),
        }
    }
}

fn write_loop(
    file: File,
    format: LogFormat,
    target: &Target,
    receiver: &Receiver<Message>,
    shared: &Shared,
) {
    let mut out = BufWriter::new(file);
    let mut plain = PlainText::default();
    let fail = |reason: String| {
        if let Ok(mut slot) = shared.error.lock() {
            *slot = Some(reason);
        }
        shared.failed.store(true, Ordering::Release);
    };
    let mut checked = std::time::Instant::now();
    while let Ok(message) = receiver.recv() {
        let bytes = match message {
            Message::Bytes(bytes) => bytes,
            Message::Stop => break,
        };
        if shared.failed.load(Ordering::Acquire) {
            continue;
        }
        let written = match format {
            LogFormat::Raw => out.write_all(&bytes),
            LogFormat::Plain => {
                let text = plain.feed(&bytes);
                out.write_all(text.as_bytes())
            }
        };
        if let Err(error) = written {
            fail(error.to_string());
            continue;
        }
        if checked.elapsed() >= TARGET_CHECK_INTERVAL {
            checked = std::time::Instant::now();
            if let Some(reason) = target.problem() {
                fail(reason);
            }
        }
    }
    if shared.failed.load(Ordering::Acquire) {
        return;
    }
    if let Some(reason) = target.problem() {
        fail(reason);
        return;
    }
    if let Err(error) = out.flush().and_then(|()| out.get_ref().sync_all()) {
        fail(error.to_string());
    }
}

/// Turns terminal output into the text a person reads, across chunk boundaries.
#[derive(Default)]
pub struct PlainText {
    parser: Parser,
    collect: Collect,
}

#[derive(Default)]
struct Collect {
    text: String,
}

impl Perform for Collect {
    fn print(&mut self, c: char) {
        self.text.push(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.text.push('\n'),
            b'\t' => self.text.push('\t'),
            // A carriage return moves the cursor; on its own it is not text, and
            // with the line feed after it the break is already written.
            _ => {}
        }
    }
}

impl PlainText {
    /// Parses one chunk and returns the text it added.
    pub fn feed(&mut self, bytes: &[u8]) -> String {
        self.parser.advance(&mut self.collect, bytes);
        std::mem::take(&mut self.collect.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shellune-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn a_plain_log_keeps_the_text_and_drops_colours_and_carriage_returns() {
        let dir = scratch("plain");
        let path = dir.join("out.log");
        let log = SessionLog::start(&path, LogFormat::Plain).expect("starts");
        log.write(b"\x1b[31mred\x1b[0m text\r\n");
        log.write(b"tab\there\r\n\x1b]0;title\x07after\r\n");
        let report = log.stop();
        assert_eq!(report.error, None);
        assert_eq!(report.dropped, 0);
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads"),
            "red text\ntab\there\nafter\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_escape_sequence_cut_between_chunks_is_still_removed() {
        let mut plain = PlainText::default();
        let mut text = plain.feed(b"a\x1b[3");
        text.push_str(&plain.feed(b"1mb\x1b"));
        text.push_str(&plain.feed(b"[0mc"));
        assert_eq!(text, "abc");
        // A UTF-8 character cut in two is one character.
        let mut text = plain.feed("é".as_bytes().split_at(1).0);
        text.push_str(&plain.feed("é".as_bytes().split_at(1).1));
        assert_eq!(text, "é");
    }

    #[test]
    fn a_raw_log_is_byte_for_byte_what_the_terminal_received() {
        let dir = scratch("raw");
        let path = dir.join("out.raw");
        let log = SessionLog::start(&path, LogFormat::Raw).expect("starts");
        let chunks: [&[u8]; 3] = [b"\x1b[31mred\x1b[0m\r\n", b"\xff\x00bytes", b"\x1b]0;t\x07"];
        for chunk in chunks {
            log.write(chunk);
        }
        let report = log.stop();
        assert_eq!(
            report.received,
            chunks.iter().map(|c| c.len() as u64).sum::<u64>()
        );
        assert_eq!(std::fs::read(&path).expect("reads"), chunks.concat());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_that_cannot_be_created_starts_nothing() {
        let dir = scratch("missing");
        let path = dir.join("no-such-dir").join("out.log");
        assert!(SessionLog::start(&path, LogFormat::Plain).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dropped_log_still_writes_what_it_was_given() {
        let dir = scratch("drop");
        let path = dir.join("out.log");
        {
            let log = SessionLog::start(&path, LogFormat::Raw).expect("starts");
            log.write(b"before close");
        }
        assert_eq!(std::fs::read(&path).expect("reads"), b"before close");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Writes until the log reports a problem, or gives up after a few seconds.
    fn write_until_failed(log: &mut SessionLog) -> Option<String> {
        for _ in 0..100 {
            log.write(b"more output\r\n");
            if let Some(found) = log.poll_error() {
                return Some(found);
            }
            std::thread::sleep(TARGET_CHECK_INTERVAL / 5);
        }
        None
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_made_read_only_while_logging_stops_the_log() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("readonly");
        let folder = dir.join("logs");
        std::fs::create_dir_all(&folder).expect("a folder");
        let mut log = SessionLog::start(&folder.join("out.log"), LogFormat::Plain).expect("starts");
        log.write(b"before\r\n");
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555))
            .expect("the folder is made read-only");
        let reason = write_until_failed(&mut log);
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755))
            .expect("the folder is writable again");
        let reason = reason.expect("the log noticed");
        assert!(reason.contains("no longer accepts writes"), "{reason}");
        assert!(log.stop().error.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_whose_folder_was_deleted_stops_instead_of_writing_into_nothing() {
        let dir = scratch("deleted");
        let folder = dir.join("logs");
        std::fs::create_dir_all(&folder).expect("a folder");
        let mut log = SessionLog::start(&folder.join("out.log"), LogFormat::Raw).expect("starts");
        log.write(b"before");
        if std::fs::remove_dir_all(&folder).is_err() {
            // A platform that refuses to delete an open file's folder keeps it.
            drop(log.stop());
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let reason = write_until_failed(&mut log).expect("the log noticed");
        assert!(reason.contains("no longer exists"), "{reason}");
        let report = log.stop();
        assert!(
            report.error.is_some(),
            "stopping does not claim it was saved"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_whose_file_was_removed_reports_it_when_stopped() {
        let dir = scratch("removed-at-stop");
        let path = dir.join("out.log");
        let log = SessionLog::start(&path, LogFormat::Raw).expect("starts");
        log.write(b"short");
        std::fs::remove_file(&path).expect("removed");
        assert!(
            log.stop().error.is_some(),
            "the last check before closing sees it"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_failing_disk_stops_the_log_and_reports_why_once() {
        // /dev/full accepts the open and refuses every write with "no space left".
        let log = SessionLog::start(Path::new("/dev/full"), LogFormat::Raw);
        let Ok(mut log) = log else {
            return;
        };
        let chunk = vec![b'x'; 64 * 1024];
        let mut error = None;
        for _ in 0..200 {
            log.write(&chunk);
            if let Some(found) = log.poll_error() {
                error = Some(found);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let error = error.expect("the failure is reported");
        assert!(!error.is_empty());
        assert_eq!(log.poll_error(), None, "the failure is handed out once");
        // Writing after the failure is quietly ignored, and stopping still returns.
        log.write(b"more");
        let report = log.stop();
        assert!(report.error.is_some());
    }
}
