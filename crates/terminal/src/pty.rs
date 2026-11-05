//! The local PTY: a real shell process behind a pseudo terminal (R2).
//!
//! This is the only place that starts a process. Everything here is written so
//! that a failure is reported as what it is — a missing program, a permission
//! problem, an exit code — because the alternative, a session that looks open but
//! has nothing behind it, is the failure mode the requirement calls out.
//!
//! Reading happens on a dedicated thread: a PTY read blocks, and blocking the
//! window for as long as a shell is idle would freeze the whole application. The
//! reader hands chunks to the caller through a channel, and a second thread waits
//! for the exit status so an abnormal exit is noticed even when the shell prints
//! nothing on its way out.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shellune_core::{LocalShell, SessionFailure};

use crate::metrics::GridSize;

/// Value of the TERM variable handed to the shell.
///
/// Programs use it to decide which escape sequences they may send; claiming
/// 256 colours is what makes Vim and top use colour instead of monochrome.
pub const TERM_VALUE: &str = "xterm-256color";

/// Size of one read from the PTY.
const READ_BUFFER: usize = 8192;

/// Why a shell could not be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellLaunchError {
    /// The program does not exist.
    NotFound {
        /// Program that was requested.
        program: String,
    },
    /// The program exists but the system refused to run it.
    PermissionDenied {
        /// Program that was requested.
        program: String,
    },
    /// The PTY or the process could not be created for another reason.
    Failed {
        /// Program that was requested.
        program: String,
        /// What the system reported.
        reason: String,
    },
}

impl ShellLaunchError {
    /// The program this error is about.
    #[must_use]
    pub fn program(&self) -> &str {
        match self {
            Self::NotFound { program }
            | Self::PermissionDenied { program }
            | Self::Failed { program, .. } => program,
        }
    }
}

impl std::fmt::Display for ShellLaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { program } => {
                write!(
                    formatter,
                    "the shell '{program}' was not found on this machine"
                )
            }
            Self::PermissionDenied { program } => write!(
                formatter,
                "the shell '{program}' could not be started because access was denied"
            ),
            Self::Failed { program, reason } => {
                write!(
                    formatter,
                    "the shell '{program}' could not be started: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for ShellLaunchError {}

impl ShellLaunchError {
    /// The session failure that records this launch problem.
    #[must_use]
    pub fn session_failure(&self) -> SessionFailure {
        match self {
            Self::NotFound { program } => SessionFailure::LocalShellUnavailable {
                shell: program.clone(),
            },
            Self::PermissionDenied { program } => SessionFailure::LocalShellPermissionDenied {
                shell: program.clone(),
            },
            Self::Failed { program, reason } => SessionFailure::LocalShellLaunchFailed {
                shell: program.clone(),
                reason: reason.clone(),
            },
        }
    }
}

/// How a shell process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitReport {
    /// The process ended with status 0.
    Clean,
    /// The process ended with a non-zero status.
    Code {
        /// The status the system reported.
        code: i32,
    },
    /// The process was killed by a signal.
    Signal {
        /// Name of the signal.
        signal: String,
    },
}

impl ExitReport {
    /// Classifies a status the PTY reported.
    #[must_use]
    pub fn from_status(status: &portable_pty::ExitStatus) -> Self {
        if let Some(signal) = status.signal() {
            return Self::Signal {
                signal: signal.to_owned(),
            };
        }
        let code = i32::try_from(status.exit_code()).unwrap_or(i32::MAX);
        if code == 0 {
            Self::Clean
        } else {
            Self::Code { code }
        }
    }

    /// Whether the session ended in a way the user should be told about.
    #[must_use]
    pub fn is_abnormal(&self) -> bool {
        !matches!(self, Self::Clean)
    }

    /// The session failure this exit implies, if it is not a clean exit.
    #[must_use]
    pub fn session_failure(&self) -> Option<SessionFailure> {
        match self {
            Self::Clean => None,
            Self::Code { code } => Some(SessionFailure::ProcessExited { code: Some(*code) }),
            Self::Signal { signal } => Some(SessionFailure::ProcessSignalled {
                signal: signal.clone(),
            }),
        }
    }
}

/// Something that happened in the shell process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyEvent {
    /// Bytes the shell produced.
    Data(Vec<u8>),
    /// The shell ended.
    Exited(ExitReport),
    /// The PTY could not be read any more.
    ReadFailed(String),
}

/// A running local shell attached to a PTY.
pub struct LocalSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    events: Receiver<PtyEvent>,
    grid: GridSize,
    closed: bool,
}

impl LocalSession {
    /// Starts a shell in a new PTY of the given size.
    ///
    /// # Errors
    ///
    /// Returns [`ShellLaunchError`] when the program is missing, not executable or
    /// when the PTY cannot be created. A session is only returned for a process that
    /// actually started.
    pub fn spawn(
        shell: &LocalShell,
        grid: GridSize,
        cwd: Option<&Path>,
    ) -> Result<Self, ShellLaunchError> {
        let mut command = CommandBuilder::new(shell.program());
        command.env("TERM", TERM_VALUE);
        command.env("COLORTERM", "truecolor");
        if let Some(directory) = cwd {
            command.cwd(directory);
        }
        Self::spawn_command(command, grid, shell.program())
    }

    /// Starts an arbitrary command in a new PTY.
    ///
    /// Used by the session itself and by tests that need a program with a known
    /// exit status; the caller is responsible for the program name in errors.
    ///
    /// # Errors
    ///
    /// As [`LocalSession::spawn`].
    pub fn spawn_command(
        command: CommandBuilder,
        grid: GridSize,
        program: &str,
    ) -> Result<Self, ShellLaunchError> {
        let grid = grid.clamped();
        // The program is checked before the PTY is created: a missing program is
        // knowable without asking the system, and the answer is unambiguous.
        // Everything that only the system can know (permissions, a broken
        // installation) still comes from the spawn error below.
        let program = command.get_argv().first().map_or_else(
            || program.to_owned(),
            |arg| arg.to_string_lossy().into_owned(),
        );
        if !crate::shell::program_exists(&program) {
            return Err(ShellLaunchError::NotFound { program });
        }
        let program = program.as_str();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(pty_size(grid))
            .map_err(|error| classify(error.as_ref(), program))?;

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| classify(error.as_ref(), program))?;
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| classify(error.as_ref(), program))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| classify(error.as_ref(), program))?;

        let (sender, receiver) = channel();
        spawn_reader(reader, sender.clone());
        spawn_waiter(child, sender);

        Ok(Self {
            master: pair.master,
            writer,
            killer,
            events: receiver,
            grid,
            closed: false,
        })
    }

    /// Sends bytes to the shell, as if typed.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the PTY is gone, which is the moment the caller
    /// must stop believing input still reaches the shell.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Tells the shell the window changed size.
    ///
    /// # Errors
    ///
    /// Returns a description when the PTY rejected the size, so a resize that did
    /// not reach the shell is not reported as done.
    pub fn resize(&mut self, grid: GridSize) -> Result<(), String> {
        let grid = grid.clamped();
        if grid == self.grid {
            return Ok(());
        }
        self.master
            .resize(pty_size(grid))
            .map_err(|error| error.to_string())?;
        self.grid = grid;
        Ok(())
    }

    /// Every event that arrived since the last call.
    pub fn drain(&self) -> Vec<PtyEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        events
    }

    /// Shuts the shell down.
    ///
    /// Killing is best effort: a process that already exited must not turn a clean
    /// close into an error.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.killer.kill();
    }

    /// Whether [`LocalSession::close`] was called.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The size the shell was last told about.
    #[must_use]
    pub fn grid(&self) -> GridSize {
        self.grid
    }
}

impl Drop for LocalSession {
    fn drop(&mut self) {
        self.close();
    }
}

fn pty_size(grid: GridSize) -> PtySize {
    PtySize {
        rows: u16::try_from(grid.rows).unwrap_or(u16::MAX),
        cols: u16::try_from(grid.cols).unwrap_or(u16::MAX),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, sender: Sender<PtyEvent>) {
    thread::Builder::new()
        .name("shellune-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0u8; READ_BUFFER];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if sender
                            .send(PtyEvent::Data(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        // A closed PTY is how a normal exit ends; only a real
                        // error is worth reporting.
                        if error.kind() != std::io::ErrorKind::Other {
                            let _ = sender.send(PtyEvent::ReadFailed(error.to_string()));
                        }
                        break;
                    }
                }
            }
        })
        .expect("the PTY reader thread can be created");
}

fn spawn_waiter(mut child: Box<dyn portable_pty::Child + Send + Sync>, sender: Sender<PtyEvent>) {
    thread::Builder::new()
        .name("shellune-pty-waiter".to_owned())
        .spawn(move || {
            let event = match child.wait() {
                Ok(status) => PtyEvent::Exited(ExitReport::from_status(&status)),
                Err(error) => {
                    PtyEvent::ReadFailed(format!("the shell exit status is unknown: {error}"))
                }
            };
            let _ = sender.send(event);
        })
        .expect("the PTY waiter thread can be created");
}

/// Turns a spawn error into the reason the user needs.
fn classify(error: &(dyn std::error::Error + 'static), program: &str) -> ShellLaunchError {
    let mut current = Some(error);
    while let Some(source) = current {
        if let Some(io) = source.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::NotFound => ShellLaunchError::NotFound {
                    program: program.to_owned(),
                },
                std::io::ErrorKind::PermissionDenied => ShellLaunchError::PermissionDenied {
                    program: program.to_owned(),
                },
                _ => ShellLaunchError::Failed {
                    program: program.to_owned(),
                    reason: io.to_string(),
                },
            };
        }
        current = source.source();
    }
    // Windows reports a failed CreateProcess as text, not as an io::Error, so the
    // last resort is to read the exit reason the system put there.
    let message = error.to_string();
    let lower = message.to_lowercase();
    if lower.contains("os error 2")
        || lower.contains("was not found")
        || lower.contains("cannot find the file")
    {
        return ShellLaunchError::NotFound {
            program: program.to_owned(),
        };
    }
    if lower.contains("os error 5") || lower.contains("access is denied") {
        return ShellLaunchError::PermissionDenied {
            program: program.to_owned(),
        };
    }
    ShellLaunchError::Failed {
        program: program.to_owned(),
        reason: message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn shell(program: &str) -> LocalShell {
        LocalShell::new(program, program)
    }

    /// Collects events until the shell exits or the deadline passes.
    fn collect_until_exit(session: &LocalSession) -> (Vec<u8>, Option<ExitReport>) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut output = Vec::new();
        loop {
            for event in session.drain() {
                match event {
                    PtyEvent::Data(bytes) => output.extend_from_slice(&bytes),
                    PtyEvent::Exited(report) => return (output, Some(report)),
                    PtyEvent::ReadFailed(reason) => panic!("the PTY died: {reason}"),
                }
            }
            if Instant::now() > deadline {
                return (output, None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_missing_program_is_reported_as_missing() {
        let error = LocalSession::spawn(
            &shell("shellune-no-such-shell-2f4a"),
            GridSize::initial(),
            None,
        )
        .err()
        .expect("a missing program must not start a session");
        assert_eq!(
            error,
            ShellLaunchError::NotFound {
                program: "shellune-no-such-shell-2f4a".to_owned()
            }
        );
        assert_eq!(
            error.session_failure(),
            SessionFailure::LocalShellUnavailable {
                shell: "shellune-no-such-shell-2f4a".to_owned()
            }
        );
    }

    #[test]
    fn a_permission_error_is_kept_apart_from_a_missing_program() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let error = classify(&denied, "/opt/locked/shell");
        assert_eq!(
            error,
            ShellLaunchError::PermissionDenied {
                program: "/opt/locked/shell".to_owned()
            }
        );
        assert!(matches!(
            error.session_failure(),
            SessionFailure::LocalShellPermissionDenied { .. }
        ));
        assert!(
            error.to_string().contains("access was denied"),
            "the message must name the real problem: {error}"
        );
    }

    #[test]
    fn an_unclassified_error_keeps_the_system_message() {
        let error = std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy);
        let error = classify(&error, "bash");
        assert!(matches!(error, ShellLaunchError::Failed { .. }));
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn a_status_with_a_signal_is_an_abnormal_exit() {
        let report = ExitReport::from_status(&portable_pty::ExitStatus::with_signal("SIGKILL"));
        assert_eq!(
            report,
            ExitReport::Signal {
                signal: "SIGKILL".to_owned()
            }
        );
        assert!(report.is_abnormal());
        assert_eq!(
            report.session_failure(),
            Some(SessionFailure::ProcessSignalled {
                signal: "SIGKILL".to_owned()
            })
        );
    }

    #[test]
    fn a_zero_status_is_a_clean_exit() {
        let report = ExitReport::from_status(&portable_pty::ExitStatus::with_exit_code(0));
        assert_eq!(report, ExitReport::Clean);
        assert!(!report.is_abnormal());
        assert_eq!(report.session_failure(), None);
    }

    #[test]
    fn a_non_zero_status_is_reported_with_its_code() {
        let report = ExitReport::from_status(&portable_pty::ExitStatus::with_exit_code(3));
        assert_eq!(report, ExitReport::Code { code: 3 });
        assert_eq!(
            report.session_failure(),
            Some(SessionFailure::ProcessExited { code: Some(3) })
        );
    }

    #[cfg(windows)]
    fn echo_program() -> (&'static str, CommandBuilder) {
        let mut command = CommandBuilder::new("cmd.exe");
        command.args(["/c", "echo hello-from-pty"]);
        ("cmd.exe", command)
    }

    #[cfg(unix)]
    fn echo_program() -> (&'static str, CommandBuilder) {
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", "echo hello-from-pty"]);
        ("sh", command)
    }

    #[test]
    fn a_real_process_writes_to_the_pty_and_exits_cleanly() {
        let (program, command) = echo_program();
        let mut session = LocalSession::spawn_command(command, GridSize::initial(), program)
            .expect("a system shell is available");

        let (output, exit) = collect_until_exit(&session);
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("hello-from-pty"),
            "the shell output did not reach the PTY: {text:?}"
        );
        assert_eq!(exit, Some(ExitReport::Clean));
        session.close();
    }

    #[cfg(windows)]
    fn exit_program(code: &str) -> (&'static str, CommandBuilder) {
        let mut command = CommandBuilder::new("cmd.exe");
        command.args(["/c", &format!("exit {code}")]);
        ("cmd.exe", command)
    }

    #[cfg(unix)]
    fn exit_program(code: &str) -> (&'static str, CommandBuilder) {
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", &format!("exit {code}")]);
        ("sh", command)
    }

    #[test]
    fn an_abnormal_exit_reports_its_real_status() {
        let (program, command) = exit_program("7");
        let mut session = LocalSession::spawn_command(command, GridSize::initial(), program)
            .expect("a system shell is available");

        let (_, exit) = collect_until_exit(&session);
        assert_eq!(exit, Some(ExitReport::Code { code: 7 }));
        session.close();
    }

    #[test]
    fn input_reaches_the_process_and_the_size_reaches_the_pty() {
        let (program, command) = if cfg!(windows) {
            let mut command = CommandBuilder::new("cmd.exe");
            command.args(["/v:on", "/c", "set /p answer=& call echo got:%answer%"]);
            ("cmd.exe", command)
        } else {
            let mut command = CommandBuilder::new("sh");
            command.args(["-c", "read answer; echo \"got:$answer\""]);
            ("sh", command)
        };
        let mut session = LocalSession::spawn_command(command, GridSize::initial(), program)
            .expect("a system shell is available");
        session
            .resize(GridSize::new(100, 30))
            .expect("the PTY accepts a resize");
        assert_eq!(session.grid(), GridSize::new(100, 30));
        session
            .write(b"typed-answer\r")
            .expect("writing to a live PTY succeeds");

        let (output, exit) = collect_until_exit(&session);
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("got:typed-answer"),
            "the typed line did not reach the process: {text:?}"
        );
        assert_eq!(exit, Some(ExitReport::Clean));
        session.close();
    }
}
