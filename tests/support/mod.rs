//! The machinery the interrupt tests share (R8).
//!
//! Two integration-test binaries drive a real Ctrl+C into a real pseudo console, and
//! both need the same three things: a way to tell whether the control byte was
//! delivered, a way to measure that the output really stopped, and a way to say out
//! loud that this machine did not deliver the byte at all instead of passing silently.
//!
//! # Why the marker decides, and the byte count only measures
//!
//! Measured on the review machine: the same test binary fails 6 of 8 runs under
//! `cargo test` while the identical binary run directly passes 4/4, and a standalone
//! local-session probe passes 5/5. The output rate alone cannot distinguish the two
//! states - "the byte never reached the program" and "it arrived and the program kept
//! printing" are both a stream that never stops - so the flood program reports the
//! interrupt it receives ([support::INTERRUPT_MARKER], see
//! `src/bin/shellune-flood.rs`), and every assertion here hangs on that report:
//!
//! * marker and output stopped: the product wrote a byte the program acted on;
//! * marker and output continuing: delivered but not stopped, a real failure - that is
//!   what `--ignore-interrupt` produces and what the negative control asserts;
//! * no marker after a bounded number of presses: the runner never delivered the byte,
//!   which is reported with the measured byte counts and skipped explicitly. It is
//!   never counted as a pass, and never as a failure of the product either.

#![allow(dead_code)]

use std::time::{Duration, Instant};

/// What the flood program prints when the console hands it a Ctrl+C.
///
/// Duplicated from `src/bin/shellune-flood.rs` rather than imported: an integration
/// test cannot link the binary it starts, and a marker that has to be read out of a
/// grid belongs next to the code that reads it.
pub const INTERRUPT_MARKER: &str = "flood: interrupted";

/// How long the byte counter must stay unchanged before output counts as stopped.
///
/// The counter is the only honest signal: the grid holds ~29 lines, so a screen that
/// is merely full and a screen that stopped both look static. The flood prints every
/// 5 ms, so one second of a running flood is not reachable by scheduling alone - a flat
/// counter over the whole window, seen by a reader that kept running (below), is the
/// stop.
pub const STOP_WINDOW: Duration = Duration::from_secs(1);

/// How many reads the stop window must have contained.
///
/// A single starved `sleep` could span the whole window; this is what turns the flat
/// counter into a measurement instead of an assumption. The loop reads every
/// [READ_INTERVAL], so an idle machine shows ~100 of them and a thread that never woke
/// up shows none.
pub const STOP_WINDOW_READS: u32 = 40;

/// How many of those windows the silence must last before it counts as a stop.
///
/// One window is not proof on a busy machine: a console host that is not being
/// scheduled holds its output back, which looks exactly like a stop and is not one.
/// Two windows cost one more second and are not the same thing as a wider window: the
/// window is still one second, and it is measured twice without a single byte in
/// between.
pub const STOP_WINDOWS: u32 = 2;

/// How often the reader looks for output while waiting.
pub const READ_INTERVAL: Duration = Duration::from_millis(10);

/// Bytes the flood must have produced before the interrupt is sent.
///
/// Not a delay: it proves the program is really mid-stream, which is the moment a user
/// interrupts it. The flood prints one line of about ten bytes every 5 ms, so two
/// kilobytes is roughly a second of stream, and the endless program cannot end by
/// itself and make a test pass without an interrupt.
pub const OUTPUT_BEFORE_INTERRUPT: u64 = 2048;

/// How long the flood is given to produce those bytes.
///
/// Generous because a busy machine slows the console host, not the program: with a cold
/// cargo build running, one measured run printed only 1791 bytes in thirty seconds
/// while the same loop printed thousands per second on an idle one. This is the
/// start-of-flood budget only; the stop window above is never widened for a busy
/// machine.
pub const FLOOD_BUDGET: Duration = Duration::from_secs(90);

/// How many Ctrl+C presses are tried before the byte is declared undeliverable here.
///
/// A bound, not a schedule: one press is what a user does, and the presses exist only
/// so that a single dropped byte - which this runner was measured to produce, the byte
/// written into a freshly created pseudo console - does not decide the outcome. When
/// three presses over their own observation windows all go unseen, the reading is that
/// this runner is not delivering the byte, and that is reported as such.
pub const PRESS_LIMIT: u32 = 3;

/// How long each press is watched for the program's marker.
///
/// Long enough for a console host that is not being scheduled to get to it, short
/// enough that a truly undelivered byte costs seconds rather than the whole budget.
pub const PRESS_OBSERVE: Duration = Duration::from_secs(4);

/// The most time the stop is watched for once the marker has arrived.
///
/// The marker is the last thing the program prints before it exits, so a delivery that
/// is not followed by silence is a program that was told to stop and did not - a real
/// failure, and the negative control's expected outcome. Wide because a console host
/// under a cold cargo build is not scheduled for long stretches.
pub const STOP_BUDGET: Duration = Duration::from_secs(10);

/// One reading of a session: how many bytes it has produced, and its grid as text.
///
/// Growth is a count of every byte, not of visible lines, so a full screen still shows
/// progress; the text is where the program's marker is looked for.
pub struct Sample {
    pub bytes: u64,
    pub text: String,
}

/// A session a control byte can be pressed into.
pub trait Flood {
    /// Reads whatever arrived and returns the current state.
    fn sample(&mut self) -> Sample;
    /// Writes one Ctrl+C, exactly as the application's Ctrl+C does.
    fn press(&mut self);
}

/// What one attempt at interrupting a flood measured.
pub enum Outcome {
    /// The program saw the byte and its output stopped: the interrupt works.
    Stopped {
        /// Bytes the session had produced at the press that was delivered.
        bytes_at_press: u64,
        /// How long after that press the output went quiet.
        stopped_after: Duration,
        /// How many presses were spent before one was delivered.
        presses: u32,
    },
    /// The program saw the byte and kept printing: delivered, not stopped.
    ///
    /// This is what `--ignore-interrupt` produces by design, and what a product that
    /// does not stop on Ctrl+C would produce.
    StillPrinting {
        bytes_at_press: u64,
        bytes_now: u64,
        presses: u32,
    },
    /// No press produced the marker: this runner did not deliver the byte.
    Undeliverable {
        presses: u32,
        bytes_at_first_press: u64,
        bytes_now: u64,
    },
}

impl Outcome {
    /// Whether the bytes were delivered to the program at all.
    pub fn delivered(&self) -> bool {
        !matches!(self, Outcome::Undeliverable { .. })
    }
}

/// Presses Ctrl+C into `flood` and reports what the program did about it.
///
/// Waits for the stream to be under way, then presses and watches for the marker; a
/// press that produces no marker inside [PRESS_OBSERVE] is followed by another, up to
/// [PRESS_LIMIT]. Delivered presses are then measured against the stop windows.
pub fn interrupt_flood(flood: &mut impl Flood) -> Outcome {
    let started = Instant::now();
    let mut sample = flood.sample();
    while sample.bytes < OUTPUT_BEFORE_INTERRUPT {
        assert!(
            started.elapsed() < FLOOD_BUDGET,
            "the flood produced only {} bytes in {:?}, so there was nothing to interrupt",
            sample.bytes,
            started.elapsed()
        );
        std::thread::sleep(READ_INTERVAL);
        sample = flood.sample();
    }

    let mut bytes_at_first_press = sample.bytes;
    for press in 1..=PRESS_LIMIT {
        if press == 1 {
            bytes_at_first_press = sample.bytes;
        }
        flood.press();
        let pressed_at = Instant::now();
        loop {
            sample = flood.sample();
            if sample.text.contains(INTERRUPT_MARKER) {
                return watch_for_stop(flood, sample, bytes_at_first_press, press, pressed_at);
            }
            if pressed_at.elapsed() >= PRESS_OBSERVE {
                break;
            }
            std::thread::sleep(READ_INTERVAL);
        }
    }

    Outcome::Undeliverable {
        presses: PRESS_LIMIT,
        bytes_at_first_press,
        bytes_now: sample.bytes,
    }
}

/// Measures whether the output that was told to stop really stops.
fn watch_for_stop(
    flood: &mut impl Flood,
    mut sample: Sample,
    bytes_at_press: u64,
    presses: u32,
    pressed_at: Instant,
) -> Outcome {
    let needed = STOP_WINDOW * STOP_WINDOWS;
    let reads_needed = STOP_WINDOW_READS * STOP_WINDOWS;
    let mut last_bytes = sample.bytes;
    let mut last_growth = Instant::now();
    let mut reads_since_growth: u32 = 0;
    loop {
        if sample.bytes != last_bytes {
            last_bytes = sample.bytes;
            last_growth = Instant::now();
            reads_since_growth = 0;
        } else {
            reads_since_growth += 1;
        }
        if last_growth.elapsed() >= needed && reads_since_growth >= reads_needed {
            return Outcome::Stopped {
                bytes_at_press,
                stopped_after: last_growth.duration_since(pressed_at),
                presses,
            };
        }
        if pressed_at.elapsed() >= STOP_BUDGET {
            return Outcome::StillPrinting {
                bytes_at_press,
                bytes_now: sample.bytes,
                presses,
            };
        }
        std::thread::sleep(READ_INTERVAL);
        sample = flood.sample();
    }
}

/// Serialises the interrupt tests of one test binary.
///
/// Each of them floods its own pseudo console and then measures whether the control
/// byte came back out as an interrupt. Run in parallel on one machine they starve each
/// other - the console host is busy printing for another session - and an undelivered
/// press is indistinguishable from a product that does not interrupt. Holding this
/// lock is what keeps the measurement about the product rather than the scheduler.
pub fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A test that panicked while holding it must not poison the rest of the file: the
    // lock protects a machine resource, not shared state a panic could leave torn.
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Writes one line to the run's log, even from a test that passes.
///
/// `eprintln!` goes through the harness's output capture and is only shown when a test
/// fails, which would make an explicit skip indistinguishable from a silent pass. This
/// writes to the stderr handle directly, which the capture does not intercept, so the
/// measured numbers and the reason for a skip stay visible in `cargo test` output.
pub fn report(label: &str, line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{label}: {line}");
}
