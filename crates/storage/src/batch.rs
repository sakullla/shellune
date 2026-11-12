//! The files a batch task and its runs live in (R19).
//!
//! Three things are written, and each says something different:
//!
//! * `batch/tasks.json` holds the saved definitions. Opening it starts nothing: a
//!   task is a definition and its targets are already snapshots, so reading the file
//!   cannot dial, cannot open a session and cannot run a command.
//! * `batch/runs/<run>.json` holds the summary of one run: the frozen task, the state
//!   of every host and of every step, plus the exit result. The result list reads this.
//! * `batch/runs/<run>.jsonl` is the append-only log: one JSON object per line, one
//!   line per thing that happened. It is appended rather than rewritten, so a run's
//!   output is never held in memory as one value, and a crash cannot lose the lines
//!   that were already written.
//!
//! A summary is the fold of its log (the fold lives in `shellune_core::BatchRun::apply`),
//! which is why the two can be rebuilt from each other and cannot disagree about what
//! happened. A restart marks every run that was still going as interrupted; nothing is
//! replayed.
//!
//! A log line that was cut off by a crash is tolerated when the log is read back: the
//! last incomplete line is reported as a torn tail and skipped, while damage anywhere
//! else is corruption and is reported instead of being guessed at.

use std::io::{ErrorKind, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shellune_core::{
    BATCH_FORMAT_VERSION, BatchBook, BatchEvent, BatchEventKind, BatchRun, BatchRunId, RunState,
};

use crate::error::PersistError;
use crate::store::{load_versioned, save_versioned};

/// Directory, inside the data directory, that holds the tasks and the runs.
pub const DIRECTORY: &str = "batch";

/// Name of the task document inside that directory.
pub const TASKS_FILE_NAME: &str = "tasks.json";

/// Directory, inside the batch directory, that holds one summary and one log per run.
pub const RUNS_DIRECTORY: &str = "runs";

/// How much of a log's end is read at a time while the store looks for its last line.
///
/// Only the sequence number of the last line is needed to append the next one, so the
/// store reads the end of the file instead of the whole log: a run with thousands of
/// hosts must not become slower with every line it writes.
///
/// The window is a read size, never a limit on a line. A line longer than the window
/// is read one window at a time until its start is found, because a long line is still
/// a line that was written in full.
const TAIL_WINDOW: u64 = 8 * 1024;

const TASKS_LABEL: &str = "batch tasks";
const RUN_LABEL: &str = "batch run";
const LOG_LABEL: &str = "batch run log";

/// The batch tasks and the runs of this data directory.
#[derive(Clone, Debug)]
pub struct BatchStore {
    root: PathBuf,
}

impl BatchStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A store beside the hosts, the workspace and the preferences.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(crate::store::data_directory())
    }

    /// The directory the batch files live in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory that holds the task document and the run directory.
    #[must_use]
    pub fn directory(&self) -> PathBuf {
        self.root.join(DIRECTORY)
    }

    /// The task document.
    #[must_use]
    pub fn tasks_path(&self) -> PathBuf {
        self.directory().join(TASKS_FILE_NAME)
    }

    /// The directory that holds one summary and one log per run.
    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.directory().join(RUNS_DIRECTORY)
    }

    /// The summary of one run.
    #[must_use]
    pub fn run_path(&self, run: BatchRunId) -> PathBuf {
        self.runs_dir().join(format!("{run}.json"))
    }

    /// The append-only log of one run.
    #[must_use]
    pub fn log_path(&self, run: BatchRunId) -> PathBuf {
        self.runs_dir().join(format!("{run}.jsonl"))
    }

    /// Reads the saved tasks.
    ///
    /// A missing file is a first run and yields an empty book. Reading starts nothing.
    ///
    /// # Errors
    ///
    /// Returns the read failure, a damaged or newer document, or a stored task this
    /// build would not have written.
    pub fn load_tasks(&self) -> Result<BatchBook, PersistError> {
        load_versioned(
            &self.directory(),
            TASKS_FILE_NAME,
            BATCH_FORMAT_VERSION,
            TASKS_LABEL,
            |book: &BatchBook| book.validate(),
        )
    }

    /// Writes the saved tasks atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, encode, write or replace failure; the previous file stays
    /// as it was.
    pub fn save_tasks(&self, book: &BatchBook) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(book).map_err(|error| PersistError::Encode {
            label: TASKS_LABEL,
            reason: error.to_string(),
        })?;
        save_versioned(&self.directory(), TASKS_FILE_NAME, TASKS_LABEL, &bytes)
    }

    /// Writes the summary of one run atomically.
    ///
    /// It is written on every state change the result list shows, and it is the file a
    /// restart reads to decide what was interrupted.
    ///
    /// # Errors
    ///
    /// Returns a directory, encode, write or replace failure.
    pub fn save_run(&self, run: &BatchRun) -> Result<(), PersistError> {
        let document = RunDocument {
            version: BATCH_FORMAT_VERSION,
            run: run.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|error| PersistError::Encode {
            label: RUN_LABEL,
            reason: error.to_string(),
        })?;
        save_versioned(
            &self.runs_dir(),
            &format!("{}.json", run.id()),
            RUN_LABEL,
            &bytes,
        )
    }

    /// Reads the summary of one run.
    ///
    /// # Errors
    ///
    /// Returns the read failure, a damaged or newer document, or a summary whose
    /// hosts and steps do not match its task.
    pub fn load_run(&self, run: BatchRunId) -> Result<BatchRun, PersistError> {
        let path = self.run_path(run);
        let text = std::fs::read_to_string(&path).map_err(|error| PersistError::Read {
            label: RUN_LABEL,
            path: path.clone(),
            reason: error.to_string(),
        })?;
        let document: RunDocument =
            serde_json::from_str(&text).map_err(|error| PersistError::Corrupt {
                label: RUN_LABEL,
                path: path.clone(),
                reason: error.to_string(),
            })?;
        if document.version > BATCH_FORMAT_VERSION {
            return Err(PersistError::UnsupportedVersion {
                label: RUN_LABEL,
                path,
                found: document.version,
                supported: BATCH_FORMAT_VERSION,
            });
        }
        document
            .run
            .validate()
            .map_err(|problem| PersistError::Corrupt {
                label: RUN_LABEL,
                path: self.run_path(run),
                reason: problem.to_string(),
            })?;
        Ok(document.run)
    }

    /// Every run this store has a summary for, newest first.
    ///
    /// # Errors
    ///
    /// Returns the directory read failure. A missing directory is a store that never
    /// ran a task and yields no runs.
    pub fn list_runs(&self) -> Result<Vec<BatchRunId>, PersistError> {
        let directory = self.runs_dir();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(PersistError::Read {
                    label: RUN_LABEL,
                    path: directory,
                    reason: error.to_string(),
                });
            }
        };
        let mut runs: Vec<(u64, BatchRunId)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| PersistError::Read {
                label: RUN_LABEL,
                path: directory.clone(),
                reason: error.to_string(),
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(run_id) = BatchRunId::parse(stem) else {
                continue;
            };
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            runs.push((modified, run_id));
        }
        runs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        Ok(runs.into_iter().map(|(_, run)| run).collect())
    }

    /// Appends one line to a run's log and returns the event it wrote.
    ///
    /// The sequence number continues from the last complete line, so a caller that
    /// emits an event twice still produces a monotonic log. The line is flushed to the
    /// operating system before the call returns, which is what makes the log the part
    /// of a run that survives a crash.
    ///
    /// # Errors
    ///
    /// Returns a directory, encode, prior-log or write failure. A refused append
    /// leaves the log as it was.
    pub fn append(
        &self,
        run: BatchRunId,
        kind: BatchEventKind,
        at_unix: u64,
    ) -> Result<BatchEvent, PersistError> {
        let directory = self.runs_dir();
        std::fs::create_dir_all(&directory).map_err(|error| PersistError::CreateDirectory {
            label: LOG_LABEL,
            path: directory.clone(),
            reason: error.to_string(),
        })?;
        let path = self.log_path(run);
        let (mut file, next) = open_for_append(&path)?;
        let event = BatchEvent::new(next, at_unix, kind);
        let mut line = serde_json::to_string(&event).map_err(|error| PersistError::Encode {
            label: LOG_LABEL,
            reason: error.to_string(),
        })?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|error| PersistError::Write {
                label: LOG_LABEL,
                path,
                reason: error.to_string(),
            })?;
        Ok(event)
    }

    /// Reads a run's log back, tolerating a last line that a crash cut off.
    ///
    /// # Errors
    ///
    /// Returns the read failure, or corruption for damage that is not the last line.
    pub fn read_events(&self, run: BatchRunId) -> Result<RunLog, PersistError> {
        let path = self.log_path(run);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(RunLog::default()),
            Err(error) => {
                return Err(PersistError::Read {
                    label: LOG_LABEL,
                    path,
                    reason: error.to_string(),
                });
            }
        };
        // A crash can cut a line in the middle of a character, so the bytes are
        // decoded leniently: a fragment that is not even UTF-8 is still only the tail
        // this reader skips, while damage before it still has to parse as an event.
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<&str> = text
            .split('\n')
            .enumerate()
            .filter_map(|(index, line)| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                Some((index, line))
            })
            .map(|(_, line)| line)
            .collect();
        let last = lines.len().saturating_sub(1);
        let mut events = Vec::with_capacity(lines.len());
        let mut tail_ignored = false;
        for (index, line) in lines.drain(..).enumerate() {
            match serde_json::from_str::<BatchEvent>(line) {
                Ok(event) => events.push(event),
                Err(error) if index == last => {
                    tail_ignored = true;
                    let _ = error;
                }
                Err(error) => {
                    return Err(PersistError::Corrupt {
                        label: LOG_LABEL,
                        path,
                        reason: format!("line {} is not an event: {error}", index + 1),
                    });
                }
            }
        }
        Ok(RunLog {
            events,
            tail_ignored,
        })
    }

    /// Marks every unfinished run as interrupted, the way a restart does.
    ///
    /// The marking is recorded as an event *and* written into the summary, so both
    /// say the same thing after the restart. Nothing is replayed.
    ///
    /// # Errors
    ///
    /// Returns the first read, append or write failure. Runs already finished are left
    /// exactly as they were.
    pub fn recover_interrupted(&self, at_unix: u64) -> Result<Vec<BatchRunId>, PersistError> {
        let mut recovered = Vec::new();
        for run in self.list_runs()? {
            let mut summary = self.load_run(run)?;
            if summary.state().is_finished() {
                continue;
            }
            summary.mark_interrupted();
            self.append(
                run,
                BatchEventKind::RunState {
                    state: RunState::Interrupted,
                },
                at_unix,
            )?;
            self.save_run(&summary)?;
            recovered.push(run);
        }
        Ok(recovered)
    }
}

/// The offset of the last `\n` before `limit`, or `None` when there is none.
///
/// The file is read backwards one [`TAIL_WINDOW`] at a time, so a line longer than the
/// window costs one more read instead of being mistaken for a fragment.
fn newline_before(file: &mut std::fs::File, limit: u64) -> std::io::Result<Option<u64>> {
    let mut end = limit;
    let mut buffer = vec![0u8; usize::try_from(TAIL_WINDOW).unwrap_or(0)];
    while end > 0 {
        let window = end.min(TAIL_WINDOW);
        let start = end - window;
        let width = usize::try_from(window).unwrap_or(0);
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..width])?;
        if let Some(index) = buffer[..width].iter().rposition(|byte| *byte == b'\n') {
            return Ok(Some(start + index as u64));
        }
        end = start;
    }
    Ok(None)
}

/// The bytes of the last complete line that ends at `end`, where `end` is just past a
/// `\n`.
///
/// The line is read in full however long it is, because a long line was still written
/// completely. Empty lines are stepped over: the store never writes one on purpose, and
/// a reader only cares about the last event.
fn last_line_before(file: &mut std::fs::File, end: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut end = end;
    while end > 1 {
        let text_end = end - 1;
        let start = newline_before(file, text_end)?.map_or(0, |index| index + 1);
        if start < text_end {
            let mut line = vec![0u8; usize::try_from(text_end - start).unwrap_or(0)];
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(&mut line)?;
            return Ok(Some(line));
        }
        end = start;
    }
    Ok(None)
}

/// Opens a run's log for appending, repairing a line a crash cut off.
///
/// A fragment at the end of the file is not an event: it was never a complete line, so
/// it is cut off before the next line is written. Damage anywhere before the last
/// complete line is corruption and is refused, because writing after it would bury the
/// evidence of what went wrong.
///
/// The last complete line is read in full, however long it is. The window is only how
/// much is read at a time while scanning backwards, so a line longer than the window is
/// still a line, and is never refused for its length.
///
/// The answer is the open file and the sequence number the next line gets.
fn open_for_append(path: &Path) -> Result<(std::fs::File, u64), PersistError> {
    let read = |reason: String| PersistError::Read {
        label: LOG_LABEL,
        path: path.to_owned(),
        reason,
    };
    let write = |reason: String| PersistError::Write {
        label: LOG_LABEL,
        path: path.to_owned(),
        reason,
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| write(error.to_string()))?;
    let length = file
        .metadata()
        .map_err(|error| read(error.to_string()))?
        .len();
    if length == 0 {
        return Ok((file, 0));
    }
    // Where the last complete line ends. Everything before that newline was written
    // in full; only the bytes after it were cut off by a crash, so only they are lost.
    let complete =
        match newline_before(&mut file, length).map_err(|error| read(error.to_string()))? {
            Some(index) => index + 1,
            // Not one complete line: the crash left a fragment and nothing else.
            None => 0,
        };
    let mut last_seq = None;
    if let Some(bytes) =
        last_line_before(&mut file, complete).map_err(|error| read(error.to_string()))?
    {
        let text = String::from_utf8_lossy(&bytes);
        let line = text.trim();
        if !line.is_empty() {
            let event: BatchEvent =
                serde_json::from_str(line).map_err(|error| PersistError::Corrupt {
                    label: LOG_LABEL,
                    path: path.to_owned(),
                    reason: error.to_string(),
                })?;
            last_seq = Some(event.seq());
        }
    }
    if complete < length {
        file.set_len(complete)
            .map_err(|error| write(error.to_string()))?;
    }
    file.seek(SeekFrom::Start(complete))
        .map_err(|error| write(error.to_string()))?;
    Ok((file, last_seq.map_or(0, |seq| seq.saturating_add(1))))
}

/// The summary document on disk: the format it uses and the run itself.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunDocument {
    version: u32,
    run: BatchRun,
}

/// A run's log as it was read back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunLog {
    events: Vec<BatchEvent>,
    tail_ignored: bool,
}

impl RunLog {
    /// Every complete event, in the order it was appended.
    #[must_use]
    pub fn events(&self) -> &[BatchEvent] {
        &self.events
    }

    /// Whether the log ended with a line that was cut off and skipped.
    #[must_use]
    pub fn tail_ignored(&self) -> bool {
        self.tail_ignored
    }

    /// Rebuilds a run by folding this log over its summary.
    ///
    /// The result is what the log says the run was, which a caller can compare with
    /// the stored summary instead of trusting either one alone.
    ///
    /// # Errors
    ///
    /// Returns the fold problem for an event that does not fit the run it names.
    pub fn replay(&self, mut run: BatchRun) -> Result<BatchRun, shellune_core::BatchProblem> {
        for event in &self.events {
            run.apply(event)?;
        }
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use shellune_core::{
        BatchBook, BatchEventKind, BatchRunId, BatchStep, BatchTarget, BatchTask,
        ConnectionDefaults, HostId, HostState, OutputStream, RunState, StepState,
    };

    use super::*;

    fn scratch(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "shellune-batch-{}-{}-{label}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn target(name: &str) -> BatchTarget {
        BatchTarget::new(
            HostId::new(),
            name,
            "192.0.2.20",
            22,
            "deploy",
            None,
            Vec::new(),
            ConnectionDefaults::default(),
        )
        .expect("a valid target")
    }

    fn task(names: &[&str]) -> BatchTask {
        BatchTask::new(
            vec![BatchStep::new("uptime").expect("a step")],
            names.iter().map(|name| target(name)).collect(),
            2,
        )
        .expect("a valid task")
    }

    fn planned(task: &BatchTask) -> BatchRun {
        let confirmed = task
            .clone()
            .confirm(1_700_000_000)
            .expect("a confirmable task");
        BatchRun::planned(&confirmed, BatchRunId::new(), 1_700_000_001)
    }

    #[test]
    fn a_first_run_reads_as_an_empty_book_and_saving_it_creates_the_document() {
        let store = BatchStore::at(scratch("empty"));
        let book = store.load_tasks().expect("a missing file is an empty book");
        assert!(book.tasks().is_empty());
        assert!(!store.tasks_path().is_file());

        let mut book = BatchBook::new();
        book.insert(task(&["alpha"]));
        store.save_tasks(&book).expect("the book saves");
        assert!(store.tasks_path().is_file());
        let reloaded = store.load_tasks().expect("the book loads");
        assert_eq!(reloaded.tasks(), book.tasks());
    }

    #[test]
    fn a_run_summary_round_trips_and_names_the_format_it_uses() {
        let store = BatchStore::at(scratch("summary"));
        let run = planned(&task(&["alpha", "beta"]));
        store.save_run(&run).expect("the summary saves");
        let reloaded = store.load_run(run.id()).expect("the summary loads");
        assert_eq!(reloaded, run);
        let text = std::fs::read_to_string(store.run_path(run.id())).expect("the file is readable");
        assert!(text.contains(&format!("\"version\": {BATCH_FORMAT_VERSION}")));
    }

    #[test]
    fn a_summary_from_a_newer_build_is_refused_instead_of_guessed() {
        let store = BatchStore::at(scratch("newer"));
        let run = planned(&task(&["alpha"]));
        store.save_run(&run).expect("the summary saves");
        let path = store.run_path(run.id());
        let text = std::fs::read_to_string(&path).expect("the file is readable");
        std::fs::write(
            &path,
            text.replace(
                &format!("\"version\": {BATCH_FORMAT_VERSION}"),
                &format!("\"version\": {}", BATCH_FORMAT_VERSION + 1),
            ),
        )
        .expect("the file can be rewritten");
        let error = store
            .load_run(run.id())
            .expect_err("the newer format is refused");
        assert!(matches!(error, PersistError::UnsupportedVersion { .. }));
    }

    #[test]
    fn appending_continues_the_sequence_and_the_log_reads_back_in_order() {
        let store = BatchStore::at(scratch("log"));
        let run = planned(&task(&["alpha"]));
        let first = store
            .append(
                run.id(),
                BatchEventKind::RunStarted {
                    hosts: 1,
                    steps: 1,
                    concurrency: 1,
                },
                10,
            )
            .expect("the first line is written");
        let second = store
            .append(
                run.id(),
                BatchEventKind::StepStarted {
                    host: run.hosts()[0].target().host_id(),
                    step: 0,
                },
                11,
            )
            .expect("the second line is written");
        assert_eq!(first.seq(), 0);
        assert_eq!(second.seq(), 1);
        // A second store on the same directory continues the same sequence: nothing is
        // kept in memory between appends.
        let other = BatchStore::at(store.root());
        let third = other
            .append(
                run.id(),
                BatchEventKind::RunState {
                    state: RunState::Completed,
                },
                12,
            )
            .expect("the third line is written");
        assert_eq!(third.seq(), 2);
        let log = store.read_events(run.id()).expect("the log reads back");
        assert_eq!(log.events().len(), 3);
        assert!(!log.tail_ignored());
        assert_eq!(log.events()[0].seq(), 0);
        assert_eq!(log.events()[2].seq(), 2);
    }

    #[test]
    fn a_line_cut_off_by_a_crash_is_skipped_and_reported_as_a_torn_tail() {
        let store = BatchStore::at(scratch("torn"));
        let run = planned(&task(&["alpha"]));
        store
            .append(
                run.id(),
                BatchEventKind::RunStarted {
                    hosts: 1,
                    steps: 1,
                    concurrency: 1,
                },
                10,
            )
            .expect("a line is written");
        let path = store.log_path(run.id());
        let mut text = std::fs::read_to_string(&path).expect("the log is readable");
        text.push_str("{\"seq\": 1, \"at_unix\": 11, \"kind\": {\"event\": \"step_st");
        std::fs::write(&path, text).expect("the log can be rewritten");
        let log = store
            .read_events(run.id())
            .expect("the torn tail is tolerated");
        assert_eq!(log.events().len(), 1);
        assert!(log.tail_ignored());
        // The next append starts after the last complete line, not after the fragment.
        let next = store
            .append(
                run.id(),
                BatchEventKind::RunState {
                    state: RunState::Completed,
                },
                12,
            )
            .expect("the next line is written");
        assert_eq!(next.seq(), 1);
    }

    #[test]
    fn damage_before_the_last_line_is_corruption() {
        let store = BatchStore::at(scratch("damaged"));
        let run = planned(&task(&["alpha"]));
        store
            .append(
                run.id(),
                BatchEventKind::RunStarted {
                    hosts: 1,
                    steps: 1,
                    concurrency: 1,
                },
                10,
            )
            .expect("a line is written");
        let path = store.log_path(run.id());
        let mut text = std::fs::read_to_string(&path).expect("the log is readable");
        text.push_str("{\"not\": \"an event\"}\n");
        text.push_str("{\"seq\": 2, \"at_unix\": 12, \"kind\": {\"event\": \"run_state\", \"state\": \"completed\"}}\n");
        std::fs::write(&path, text).expect("the log can be rewritten");
        let error = store
            .read_events(run.id())
            .expect_err("mid-log damage is refused");
        assert!(matches!(error, PersistError::Corrupt { .. }));
    }

    /// The text a chunk of bytes that are not UTF-8 becomes in the log.
    ///
    /// Output is recorded with the bytes that are not UTF-8 replaced, and every
    /// replacement is escaped again by JSON, so one such chunk is a single line several
    /// times longer than the window the store reads.
    fn replacement_chunk(bytes: usize) -> String {
        String::from_utf8_lossy(&vec![0x80u8; bytes]).into_owned()
    }

    fn output(run: &BatchRun, chunk: String) -> BatchEventKind {
        BatchEventKind::Output {
            host: run.hosts()[0].target().host_id(),
            step: 0,
            stream: OutputStream::Stdout,
            chunk,
        }
    }

    #[test]
    fn a_line_longer_than_the_read_window_is_a_line_and_not_a_fragment() {
        let store = BatchStore::at(scratch("long-line"));
        let run = planned(&task(&["alpha"]));
        store
            .append(
                run.id(),
                BatchEventKind::RunStarted {
                    hosts: 1,
                    steps: 1,
                    concurrency: 1,
                },
                10,
            )
            .expect("the first line is written");
        let long = store
            .append(run.id(), output(&run, replacement_chunk(4 * 1024)), 11)
            .expect("a line longer than the window is still one line");
        assert_eq!(long.seq(), 1);
        let length = std::fs::metadata(store.log_path(run.id()))
            .expect("the log has a length")
            .len();
        assert!(
            length > TAIL_WINDOW,
            "the fixture line is longer than the window the store reads: {length}"
        );
        // The next append reads that line in full instead of calling its end damage.
        let next = store
            .append(
                run.id(),
                BatchEventKind::RunState {
                    state: RunState::Completed,
                },
                12,
            )
            .expect("the log stays appendable after a long line");
        assert_eq!(next.seq(), 2);
        let log = store.read_events(run.id()).expect("the log reads back");
        assert_eq!(log.events().len(), 3);
        assert!(!log.tail_ignored());
        assert_eq!(log.events()[1].seq(), 1);
        assert!(matches!(
            log.events()[1].kind(),
            BatchEventKind::Output { chunk, .. } if chunk.chars().count() == 4 * 1024
        ));
    }

    #[test]
    fn a_tail_of_bytes_that_are_not_utf8_is_still_only_a_torn_tail() {
        let store = BatchStore::at(scratch("torn-bytes"));
        let run = planned(&task(&["alpha"]));
        store
            .append(
                run.id(),
                BatchEventKind::RunStarted {
                    hosts: 1,
                    steps: 1,
                    concurrency: 1,
                },
                10,
            )
            .expect("the first line is written");
        store
            .append(run.id(), output(&run, replacement_chunk(4 * 1024)), 11)
            .expect("the long line is written");
        // A crash in the middle of a character leaves bytes that are not UTF-8 at all.
        let path = store.log_path(run.id());
        let mut bytes = std::fs::read(&path).expect("the log is readable");
        bytes.extend_from_slice(
            b"{\"seq\": 2, \"at_unix\": 12, \"kind\": {\"event\": \"output\", \"chunk\": \"",
        );
        bytes.extend_from_slice(&[0xF0, 0x9F, 0x92]);
        std::fs::write(&path, &bytes).expect("the log can be rewritten");
        let log = store
            .read_events(run.id())
            .expect("the torn tail is tolerated");
        assert_eq!(log.events().len(), 2);
        assert!(log.tail_ignored());
        let next = store
            .append(
                run.id(),
                BatchEventKind::RunState {
                    state: RunState::Completed,
                },
                13,
            )
            .expect("the fragment is cut off and the line is written");
        assert_eq!(next.seq(), 2);
        let after = store.read_events(run.id()).expect("the log reads back");
        assert_eq!(after.events().len(), 3);
        assert!(!after.tail_ignored());
    }

    #[test]
    fn a_restart_appends_after_a_line_longer_than_the_read_window() {
        let store = BatchStore::at(scratch("long-recovery"));
        let running_task = task(&["alpha"]);
        let mut running = planned(&running_task);
        let host = running.hosts()[0].target().host_id();
        running
            .apply(&BatchEvent::new(
                0,
                10,
                BatchEventKind::StepStarted { host, step: 0 },
            ))
            .expect("started");
        store.save_run(&running).expect("the running run saves");
        store
            .append(
                running.id(),
                BatchEventKind::StepStarted { host, step: 0 },
                10,
            )
            .expect("the log line is written");
        store
            .append(
                running.id(),
                output(&running, replacement_chunk(4 * 1024)),
                11,
            )
            .expect("the long line is written");
        let recovered = store.recover_interrupted(30).expect("the restart recovers");
        assert_eq!(recovered, vec![running.id()]);
        assert_eq!(
            store.load_run(running.id()).expect("the run loads").state(),
            RunState::Interrupted
        );
        let log = store.read_events(running.id()).expect("the log reads back");
        assert_eq!(log.events().len(), 3);
        assert!(!log.tail_ignored());
        assert_eq!(log.events().last().map(BatchEvent::seq), Some(2));
        assert!(matches!(
            log.events().last().map(BatchEvent::kind),
            Some(BatchEventKind::RunState {
                state: RunState::Interrupted
            })
        ));
    }

    #[test]
    fn a_restart_marks_the_unfinished_run_and_leaves_the_finished_one_alone() {
        let store = BatchStore::at(scratch("restart"));
        let finished_task = task(&["alpha"]);
        let mut finished = planned(&finished_task);
        let host = finished.hosts()[0].target().host_id();
        finished
            .apply(&BatchEvent::new(
                0,
                10,
                BatchEventKind::StepStarted { host, step: 0 },
            ))
            .expect("started");
        finished
            .apply(&BatchEvent::new(
                1,
                11,
                BatchEventKind::StepFinished {
                    host,
                    step: 0,
                    state: StepState::Succeeded,
                    exit: Some(0),
                    detail: None,
                },
            ))
            .expect("finished the step");
        finished
            .apply(&BatchEvent::new(
                2,
                12,
                BatchEventKind::HostFinished {
                    host,
                    state: HostState::Succeeded,
                    detail: None,
                },
            ))
            .expect("finished the host");
        finished
            .apply(&BatchEvent::new(
                3,
                13,
                BatchEventKind::RunState {
                    state: RunState::Completed,
                },
            ))
            .expect("completed");
        store.save_run(&finished).expect("the finished run saves");

        let running_task = task(&["beta"]);
        let mut running = planned(&running_task);
        let other = running.hosts()[0].target().host_id();
        running
            .apply(&BatchEvent::new(
                0,
                20,
                BatchEventKind::StepStarted {
                    host: other,
                    step: 0,
                },
            ))
            .expect("started");
        store.save_run(&running).expect("the running run saves");
        store
            .append(
                running.id(),
                BatchEventKind::StepStarted {
                    host: other,
                    step: 0,
                },
                20,
            )
            .expect("the log line is written");

        let recovered = store.recover_interrupted(30).expect("the restart recovers");
        assert_eq!(recovered, vec![running.id()]);
        let reloaded = store.load_run(running.id()).expect("the run loads");
        assert_eq!(reloaded.state(), RunState::Interrupted);
        assert_eq!(reloaded.hosts()[0].state(), HostState::Interrupted);
        assert_eq!(
            reloaded.hosts()[0].steps()[0].state(),
            StepState::Interrupted
        );
        // The marking is in the log too, so a reader of the log sees the same thing.
        let log = store.read_events(running.id()).expect("the log reads back");
        assert!(matches!(
            log.events().last().map(BatchEvent::kind),
            Some(BatchEventKind::RunState {
                state: RunState::Interrupted
            })
        ));
        // The finished run was not touched.
        let untouched = store
            .load_run(finished.id())
            .expect("the finished run loads");
        assert_eq!(untouched.state(), RunState::Completed);
        // A second restart finds nothing to recover.
        assert!(
            store
                .recover_interrupted(31)
                .expect("the second restart runs")
                .is_empty()
        );
    }

    #[test]
    fn the_log_replays_into_the_summary_it_was_written_for() {
        let store = BatchStore::at(scratch("replay"));
        let run = planned(&task(&["alpha", "beta"]));
        let confirmed = run.task().clone().confirm(1).expect("a confirmable task");
        let mut folded = BatchRun::planned(&confirmed, run.id(), run.started_at_unix());
        let mut seq = 0;
        let mut emit = |kind: BatchEventKind| {
            store
                .append(run.id(), kind, 10)
                .expect("the line is written");
            seq += 1;
        };
        emit(BatchEventKind::RunStarted {
            hosts: 2,
            steps: 1,
            concurrency: 2,
        });
        for host in run.hosts() {
            let id = host.target().host_id();
            emit(BatchEventKind::StepStarted { host: id, step: 0 });
            emit(BatchEventKind::StepFinished {
                host: id,
                step: 0,
                state: StepState::Succeeded,
                exit: Some(0),
                detail: None,
            });
            emit(BatchEventKind::HostFinished {
                host: id,
                state: HostState::Succeeded,
                detail: None,
            });
        }
        emit(BatchEventKind::RunState {
            state: RunState::Completed,
        });
        let log = store.read_events(run.id()).expect("the log reads back");
        let replayed = log.replay(folded.clone()).expect("the log folds");
        assert_eq!(replayed.state(), RunState::Completed);
        assert_eq!(replayed.finished_hosts(), 2);
        folded
            .apply(&BatchEvent::new(
                0,
                0,
                BatchEventKind::RunStarted {
                    hosts: 2,
                    steps: 1,
                    concurrency: 2,
                },
            ))
            .expect("the summary folds too");
    }
}
