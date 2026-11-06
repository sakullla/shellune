//! A program that floods a terminal and reports the interrupt it receives (R8).
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
//! # The delivery signal
//!
//! The tests cannot tell "the Ctrl+C byte never reached this program" from "it arrived
//! and the program kept printing" by looking at the output rate alone: both look like a
//! stream that never stops. So the program installs a Ctrl+C handler that, when the
//! console hands it an interrupt, makes the program print [`INTERRUPT_MARKER`] - once -
//! and then
//!
//! * exit with status 130, the status a shell reports for an interrupted command, or
//! * keep printing, under `--ignore-interrupt`, so the negative control is a real
//!   "delivered but not stopped" rather than a program that never saw the byte.
//!
//! The handler only sets an atomic flag, and the printing loop does the writing: a
//! console handler runs on a thread the console owns, and writing to a locked handle
//! from there is asking for a deadlock. The loop notices within one 5 ms interval,
//! which is far below the second the tests measure a stop over.
//!
//! # Behaviour
//!
//! Prints `tick <n>`, one line every 5 ms, until the interrupt under test arrives; then
//! prints the marker and, unless `--ignore-interrupt` was given, exits.
//!
//! It deliberately does not read its own input: a reader would consume the console's key
//! records itself, and measured here that stopped the console from turning the byte into
//! the interrupt this program is supposed to be killed by.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// What the program prints when the console hands it a Ctrl+C.
///
/// The tests read this out of the terminal grid: seeing it is the evidence that the
/// control byte was delivered and acted on, and not merely written into a queue.
pub const INTERRUPT_MARKER: &str = "flood: interrupted";

/// How long the program waits between two lines.
///
/// Slow enough that a terminal can act on input while it arrives: at this rate the
/// output is a few KB/s, so the pseudo console's input queue is never starved.
const INTERVAL: Duration = Duration::from_millis(5);

/// The status a shell reports for a command it interrupted with Ctrl+C.
const INTERRUPTED_STATUS: i32 = 130;

/// Set by the Ctrl+C handler, read by the printing loop.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn main() {
    let ignore_interrupt = std::env::args().any(|arg| arg == "--ignore-interrupt");
    install_handler();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut tick: u64 = 0;
    let mut announced = false;
    loop {
        if !announced && INTERRUPTED.load(Ordering::Relaxed) {
            announced = true;
            let _ = writeln!(out, "{INTERRUPT_MARKER}");
            let _ = out.flush();
            if !ignore_interrupt {
                // The interrupt ended the program; the marker above already said so.
                std::process::exit(INTERRUPTED_STATUS);
            }
        }
        tick += 1;
        // A closed terminal is not an error: the end of the stream is the end of the
        // program, quietly, the way a console program exits when its pipe goes away.
        if writeln!(out, "tick {tick}").is_err() || out.flush().is_err() {
            return;
        }
        std::thread::sleep(INTERVAL);
    }
}

/// Records that the console delivered a Ctrl+C.
#[cfg(windows)]
fn install_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    /// The handler Windows calls for Ctrl+C. Answering "handled" keeps the default
    /// handler - which kills the process without a word - out of the way, so the
    /// printing loop is the one that reports the interrupt.
    unsafe extern "system" fn record(_ctrl_type: u32) -> i32 {
        INTERRUPTED.store(true, Ordering::Relaxed);
        1
    }

    // SAFETY: installing a process-wide handler before any other thread exists; the
    // handler only stores into an atomic and returns TRUE.
    unsafe {
        SetConsoleCtrlHandler(Some(record), 1);
    }
}

/// Records that the console delivered a Ctrl+C.
#[cfg(unix)]
fn install_handler() {
    /// Async-signal-safe by construction: one atomic store, nothing else.
    extern "C" fn record(_signal: libc::c_int) {
        INTERRUPTED.store(true, Ordering::Relaxed);
    }

    // SAFETY: SIGINT is process-wide state, set before any output starts, and the
    // handler does nothing a signal handler may not do.
    unsafe {
        libc::signal(libc::SIGINT, record as libc::sighandler_t);
    }
}

#[cfg(not(any(windows, unix)))]
fn install_handler() {
    // No console interrupt to record on this platform, so the flag never becomes true
    // and the tests report that the byte could not be observed instead of passing.
    eprintln!("Ctrl+C is not observable on this platform: the marker never appears");
}
