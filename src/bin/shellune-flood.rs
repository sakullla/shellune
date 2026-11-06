//! A program that floods a terminal, for the interrupt tests (R8).
//!
//! The interrupt tests used to type an endless loop into `cmd.exe`. That loop printed
//! as fast as the console host let it - about 600 KB/s - and on a reviewer's machine
//! that was fast enough that the pseudo console never got to look at the Ctrl+C byte
//! sitting in its input at all: nine presses over 24 s produced no `^C`, no prompt and
//! no stop, while the baseline's throttled loop was interrupted in ~11 ms. A test whose
//! subject is "Ctrl+C stops the output" cannot be built on a program whose output rate
//! starves the interrupt, so the flood is a program of its own now: it prints one line
//! every 5 ms and does nothing else, which is a stream a terminal can process input
//! against.
//!
//! It is a test helper, never shipped: `required-features = ["test-support"]` keeps it
//! out of every build that does not ask for it, release builds included.
//!
//! # Behaviour
//!
//! Prints `tick <n>`, one line every 5 ms, until it is interrupted. Ctrl+C terminates it
//! the platform's default way - the same thing that happens to any console program that
//! installs no handler, which is what a user interrupting a command gets.
//!
//! It deliberately does not read its own input: a reader would consume the console's key
//! records itself, and measured here that stopped the console from turning the byte into
//! the interrupt this program is supposed to be killed by.
//!
//! `--ignore-interrupt` installs a Ctrl+C handler that does nothing and keeps printing.
//! That is the negative control of the interrupt tests: a program that cannot be stopped
//! has to make the stop detection fail, otherwise the detection proves nothing.

use std::io::Write;
use std::time::Duration;

/// How long the program waits between two lines.
///
/// Slow enough that a terminal can act on input while it arrives: at this rate the
/// output is a few KB/s, so the pseudo console's input queue is never starved.
const INTERVAL: Duration = Duration::from_millis(5);

fn main() {
    if std::env::args().any(|arg| arg == "--ignore-interrupt") {
        ignore_interrupt();
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut tick: u64 = 0;
    loop {
        tick += 1;
        // A closed terminal is not an error: the end of the stream is the end of the
        // program, quietly, the way a console program exits when its pipe goes away.
        if writeln!(out, "tick {tick}").is_err() || out.flush().is_err() {
            return;
        }
        std::thread::sleep(INTERVAL);
    }
}

/// Makes Ctrl+C a no-op, so the flood cannot be stopped by the interrupt under test.
#[cfg(windows)]
fn ignore_interrupt() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    /// The handler Windows calls for Ctrl+C: answering "handled" and returning is the
    /// whole job, so the default handler never runs and the process keeps running.
    unsafe extern "system" fn handled(_ctrl_type: u32) -> i32 {
        1
    }

    // SAFETY: installing a process-wide handler before any other thread exists; the
    // handler only returns TRUE and touches nothing.
    unsafe {
        SetConsoleCtrlHandler(Some(handled), 1);
    }
}

/// Makes Ctrl+C a no-op, so the flood cannot be stopped by the interrupt under test.
#[cfg(unix)]
fn ignore_interrupt() {
    // SAFETY: SIG_IGN for SIGINT is process-wide state, set before any output starts;
    // nothing else in this program changes signal dispositions.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
}

#[cfg(not(any(windows, unix)))]
fn ignore_interrupt() {
    // No console interrupt to ignore on this platform, so the flag is a no-op and the
    // negative control cannot run; the tests say so instead of pretending otherwise.
    eprintln!("--ignore-interrupt is not implemented on this platform");
}
