//! The Ctrl+C tests (R8) against a real shell and a real flood program.
//!
//! This file is one binary on purpose. The cases that press a control byte into a pseudo
//! console and read the marker back are the only ones that need it, and this machine stops
//! delivering that byte once a process has created and closed the terminals of the cases
//! in local_shell.rs (tests/support/session.rs records the measurements). Kept apart, the
//! four cases really run here instead of printing a skip that exits 0; the fifth case, a
//! shell that has to keep taking input after a Ctrl+C, moved with them because it shares
//! these waits.
//!
//! Everything measured here is the product's: the byte is written into the session PTY
//! exactly as the application Ctrl+C writes it, and the marker proves the shell and the
//! program acted on it.

#![cfg(feature = "test-support")]

#[path = "support/mod.rs"]
mod support;

#[path = "support/session.rs"]
mod session;

use std::time::{Duration, Instant};

use session::Session;
use shellune_core::LocalShell;

/// The interrupt tests (R8), and the program they interrupt.
///
/// # Why the flood is a program of its own
///
/// These tests used to type an endless loop into `cmd.exe`:
/// `for /l %i in (1,1,1000000000) do @echo tick`. That loop prints as fast as the
/// console host allows - about 600 KB/s - and that rate is the real reason the tests were
/// red on machines other than this one: the pseudo console never got to look at the
/// Ctrl+C byte in its input at all, so nine presses over 24 s produced no `^C` and no
/// stop, while the baseline's loop (with a 5 ms sleep) was interrupted in ~11 ms. Widening
/// the stop window would hide that; the stream itself had to be one a terminal can act on.
///
/// The flood is therefore `shellune-flood`, a program built for these tests only
/// (`required-features = ["test-support"]`, so it is not in a release build). It prints
/// one line every 5 ms and does nothing else.
///
/// # What the byte count cannot tell, and the marker can
///
/// Under `cargo test` the same test binary was measured to fail 6 of 8 runs, while
/// that identical binary run directly passed 4 of 4 and a standalone local-session probe
/// passed 5 of 5. So in this runner the Ctrl+C is sometimes simply never acted on by the
/// console, and a test that watches only the output rate cannot tell that apart from a
/// stream that arrived and kept printing: in both, the counter never goes flat.
///
/// The flood therefore reports the interrupt it receives, by printing
/// `flood: interrupted` when its Ctrl+C handler runs, and every assertion here
/// hangs on that report:
///
/// * marker, then silence - the product wrote a byte the program acted on;
/// * marker, then more output - delivered and not stopped, which is a real failure (and
///   exactly what `--ignore-interrupt` produces for the negative control);
/// * no marker within [support::PRESS_LIMIT] bounded presses - this runner never delivered
///   the byte. That is reported with the measured byte counts and skipped out loud, never
///   passed off as a pass and never charged to the product.
///
/// # The shell path (R8's user path)
///
/// The application's Ctrl+C writes the byte into the shell's PTY and the shell forwards it
/// to the foreground program, so `the_shell_forwards_the_interrupt_to_the_program_it_started`
/// runs the same flood from a shell prompt - `cmd.exe` on Windows, the platform shell
/// elsewhere - and reads the same marker. It claims nothing unless the marker proves the
/// shell really passed the interrupt on, and says so out loud when it does not.
///
/// The earlier shape of this file avoided the shell path, on the strength of five measured
/// runs in which a byte written into `cmd.exe` was never acted on while a program it had
/// started kept printing. That program left its own termination to the console's default
/// handler; this one reports the interrupt and exits itself, and under the same shell the
/// marker now arrives on the first press (measured: ten of ten runs of this file, plus
/// twice in the full suite). Which of the two differences - the default handler being
/// deferred under a shell, or something else - explains the earlier five runs is not
/// distinguished here, so the test relies on neither explanation: it reports what the
/// marker says.
///
/// # What the numbers are, and are not
///
/// The stop window is never widened, and the retries are bounded and each verified by the
/// marker. A press that changes nothing is only retried because this runner was measured to
/// drop a byte written into a freshly created pseudo console; when all of them go unseen the
/// test says the runner did not deliver the byte instead of guessing.
#[cfg(feature = "test-support")]
mod interrupt {
    use super::*;
    use crate::support::{self, Flood, Outcome, Sample};

    /// The flood program, as Cargo built it for this test binary.
    ///
    /// Cargo exports `CARGO_BIN_EXE_<name>` to an integration test for every binary it
    /// built for it; the `required-features` on the binary is what keeps it out of a
    /// release build, and this whole module is behind the same feature.
    const FLOOD_PROGRAM: &str = env!("CARGO_BIN_EXE_shellune-flood");

    /// A session, read as bytes and text, with one Ctrl+C per press.
    ///
    /// The press is the product's: the application's Ctrl+C writes 0x03 into the session's
    /// PTY (`crates/terminal` key handling), which is the same call this makes.
    impl Flood for Session {
        fn sample(&mut self) -> Sample {
            self.pump();
            Sample {
                bytes: self.bytes_read(),
                text: self.core.snapshot().text().join("\n"),
            }
        }

        fn press(&mut self) {
            self.write(&[0x03]);
        }
    }

    /// The command that proves the shell still takes input, and how many times the
    /// marker must appear on screen for the command to have really run.
    ///
    /// A terminal echoes what is typed, so a marker inside the command line would let
    /// the echo satisfy the wait. `cmd.exe` echoes the whole line and then prints the
    /// output, so the marker appears twice only after the command ran; the POSIX branch
    /// splits the marker in the command itself, so once is already real output.
    fn follow_up_command() -> (&'static str, usize) {
        if cfg!(windows) {
            ("echo after-interrupt", 2)
        } else {
            ("echo 'after'-'interrupt'", 1)
        }
    }

    /// How long the shell has to have written nothing before it counts as at its prompt.
    ///
    /// Long enough that a prompt which arrives in two pieces is not read as silence, short
    /// enough that the wait costs a third of a second when the shell is already idle.
    const QUIET_WINDOW: Duration = Duration::from_millis(400);

    /// How many times the launch command is written before the flood is called not started.
    ///
    /// A bound, not a schedule: the command never needs two writes on an idle machine, and
    /// three is where a console that keeps losing a line is reported instead of waited on.
    const LAUNCH_ATTEMPTS: u32 = 3;

    /// How long one write of the launch command is watched for the program's own output.
    ///
    /// The flood prints a line every 5 ms, so a started program answers in milliseconds.
    /// This window is about a lost line, not a slow machine: one that has printed nothing
    /// for this long was not started, and the line is written again.
    const LAUNCH_OBSERVE: Duration = Duration::from_secs(5);

    /// The marker typed to find out whether the shell is reading input yet.
    const INPUT_MARKER: &str = "shellune-input-check";

    /// Waits until the shell has answered a typed line.
    ///
    /// A prompt on screen proves the shell is drawing; it does not prove it is reading
    /// what arrives, and a line written before it reads is dropped without a trace. The
    /// marker is therefore offered again on a fixed interval until the shell prints it
    /// back, which is the only evidence that input really reaches the program. Two
    /// occurrences are required because the terminal echoes the typed line as well as the
    /// output, so the echo alone cannot satisfy the wait.
    fn wait_until_it_takes_input(session: &mut Session) {
        let deadline = Instant::now() + support::FLOOD_BUDGET;
        let mut next_offer = Instant::now();
        let mut offers = 0;
        let mut answered_at_last_offer = 0;
        loop {
            session.pump();
            let text = session.core.snapshot().text().join("\n");
            let seen = text.matches(INPUT_MARKER).count();
            if seen >= 2 {
                return;
            }
            // One line at a time. Offered on a schedule instead, the lines pile up in the
            // console input while the shell is busy, and the command that matters - the one
            // that starts the flood - is written behind them and can be lost whole: a
            // measured full-suite run died exactly there, with four unanswered echoes on
            // screen, 10574 bytes of nothing but echoes, and no flood. So the next offer
            // waits until the previous one has been answered, which is what makes the
            // return below mean "the shell is reading input right now".
            let answered = seen > answered_at_last_offer;
            if Instant::now() >= next_offer && (offers == 0 || answered) {
                session.run(&format!("echo {INPUT_MARKER}"));
                offers += 1;
                answered_at_last_offer = seen;
                next_offer = Instant::now() + Duration::from_millis(500);
            }
            assert!(
                Instant::now() < deadline,
                "the shell never answered a typed line in {:?} although it was offered \
                 {offers} time(s); the screen held:\n{text}",
                support::FLOOD_BUDGET
            );
            std::thread::sleep(support::READ_INTERVAL);
        }
    }

    /// Waits until the shell has stopped writing, which is when it is sitting at a prompt.
    ///
    /// A shell that is still printing its banner has not read its input yet, and a console
    /// with a line editor drops what arrives before the prompt is drawn: measured here, a
    /// cmd.exe carrying Clink printed its banner after the echoes of the input checks, and
    /// all three writes of the launch command went into a shell that had not reached a
    /// prompt - the screen held the banner and 1587 bytes, no flood, no tick. The echo of a
    /// typed line arrives before the shell executes it, so "it answered" is not the same as
    /// "it is ready"; silence is, and it is the shell's own output that ends this wait.
    fn wait_until_the_shell_is_quiet(session: &mut Session) {
        let deadline = Instant::now() + support::FLOOD_BUDGET;
        let mut last = session.bytes_read();
        let mut quiet_since = Instant::now();
        loop {
            session.pump();
            let now = session.bytes_read();
            if now != last {
                last = now;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= QUIET_WINDOW {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the shell never stopped writing in {:?}, so it never reached a prompt",
                support::FLOOD_BUDGET
            );
            std::thread::sleep(support::READ_INTERVAL);
        }
    }

    /// Offers the follow-up command until the shell answers it, and returns the screen.
    ///
    /// A line typed while the shell is still clearing the interrupted line is swallowed,
    /// so the same line is offered again until the shell answers.
    fn shell_takes_a_command_after_the_interrupt(session: &mut Session) -> String {
        let (command, marker_count) = follow_up_command();
        let mut offers = 0;
        let mut next_offer = Instant::now();
        let deadline = Instant::now() + support::FLOOD_BUDGET;
        loop {
            session.pump();
            let text = session.core.snapshot().text().join("\n");
            if text.matches("after-interrupt").count() >= marker_count {
                return text;
            }
            if Instant::now() >= next_offer {
                session.write(format!("{command}\r").as_bytes());
                offers += 1;
                next_offer = Instant::now() + Duration::from_millis(250);
            }
            assert!(
                Instant::now() < deadline,
                "the shell never took a command after the Ctrl+C: {offers} follow-up \
                 command(s) were offered; the screen held:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn sustained_output_can_be_interrupted_and_the_session_survives_it() {
        let _alone = support::exclusive();
        let Some(mut session) = Session::start_program(FLOOD_PROGRAM, &[]) else {
            support::report(
                "local_interrupt",
                "SKIP sustained_output_can_be_interrupted_and_the_session_survives_it: \
                 the flood program is not available",
            );
            return;
        };

        match support::interrupt_flood(&mut session) {
            Outcome::Stopped {
                bytes_at_press,
                stopped_after,
                presses,
            } => support::report(
                "local_interrupt",
                &format!(
                    "sustained output: the flood printed {} bytes, saw the Ctrl+C on press \
                     {presses} of at most {}, and stopped after {stopped_after:?}",
                    bytes_at_press,
                    support::PRESS_LIMIT
                ),
            ),
            Outcome::StillPrinting {
                bytes_at_press,
                bytes_now,
                presses,
            } => panic!(
                "the flood saw the Ctrl+C and kept printing: {bytes_at_press} bytes at press \
                 {presses}, {bytes_now} bytes now, no {:?} of silence within {:?}",
                support::STOP_WINDOW * support::STOP_WINDOWS,
                support::STOP_BUDGET
            ),
            Outcome::Undeliverable {
                presses,
                bytes_at_first_press,
                bytes_now,
            } => {
                support::undelivered(
                    "local_interrupt",
                    "sustained_output_can_be_interrupted_and_the_session_survives_it",
                    presses,
                    bytes_at_first_press,
                    bytes_now,
                    "The product's Ctrl+C path is therefore unmeasured here, not passing.",
                );
                return;
            }
        }

        // The interrupt ended the program and nothing else: the console's Ctrl+C
        // terminates a process that installs no handler, and that is reported as the
        // program's end rather than as a session that died.
        let deadline = Instant::now() + support::FLOOD_BUDGET;
        while session.exited.is_none() {
            session.pump();
            assert!(
                Instant::now() < deadline,
                "the interrupted program never reported its end, so the silence above may \
                 have been the console host holding output back rather than a stop"
            );
            std::thread::sleep(support::READ_INTERVAL);
        }
        assert!(
            !session.pty.is_closed(),
            "the interrupt ends the program, not the session it ran in"
        );
    }

    /// A shell that took a Ctrl+C still takes a command (R8).
    ///
    /// The shell's half of the interrupt, on its own: a byte is written into an idle
    /// prompt, and the shell has to answer the next command with real output. Whether the
    /// shell forwards that byte to a program it is waiting on is the test below.
    #[test]
    fn a_shell_stays_usable_after_a_ctrl_c() {
        // The console host is shared, so this case takes it too - and it has to, not just
        // should: measured in this binary, running these tests in parallel lost the control
        // byte for all four marker cases as long as this one was creating its session at the
        // same time as a flood. Two pseudo consoles alive at once is where this runner stops
        // delivering the byte; a lock is what keeps the measurement about the product.
        let _alone = support::exclusive();
        let Some(mut session) = Session::start() else {
            support::report(
                "local_interrupt",
                "SKIP a_shell_stays_usable_after_a_ctrl_c: no shell on this machine",
            );
            return;
        };

        wait_until_it_takes_input(&mut session);
        session.write(&[0x03]);
        shell_takes_a_command_after_the_interrupt(&mut session);
    }

    /// The flood program's command line, quoted so a path with spaces still starts.
    fn flood_command() -> String {
        if cfg!(windows) {
            format!("\"{FLOOD_PROGRAM}\"")
        } else {
            FLOOD_PROGRAM.to_owned()
        }
    }

    /// The command that starts the flood from `shell`.
    ///
    /// PowerShell needs its call operator in front of a quoted path, otherwise it prints
    /// the string instead of running it; `cmd.exe` takes the quoted path directly.
    fn flood_command_for(shell: &LocalShell) -> String {
        let program = shell.program().to_ascii_lowercase();
        if cfg!(windows) && (program.contains("powershell") || program.contains("pwsh")) {
            format!("& \"{FLOOD_PROGRAM}\"")
        } else {
            flood_command()
        }
    }

    /// The shell the flood is started from: the one R8's user path goes through.
    ///
    /// `cmd.exe` on Windows (`/q` only silences command echo), the platform shell
    /// elsewhere.
    fn shell_for_the_flood() -> Option<Session> {
        if cfg!(windows) {
            Session::start_program("cmd.exe", &["/q"])
        } else {
            Session::start()
        }
    }

    /// R8's user path: a program started from a shell prompt is stopped by Ctrl+C.
    ///
    /// The application never puts a program in a PTY of its own - the session always
    /// starts a shell, and Ctrl+C is a byte the shell forwards to the foreground program -
    /// so this is the path a user's interrupt actually takes. The marker is what makes the
    /// claim: it appears only if the shell passed the byte on. If it does not appear the
    /// test reports a loud SKIP carrying the measured byte counts and says the case is
    /// unmeasured here, not passing - never a pass, because no byte was ever delivered.
    /// A runner that *can* deliver the byte (CI, a console session on a real terminal)
    /// sets `SHELLUNE_REQUIRE_INTERRUPT_DELIVERY=1` and then the same case fails instead.
    #[test]
    fn the_shell_forwards_the_interrupt_to_the_program_it_started() {
        // The console host is taken before the shell is started, not after: a session
        // created while another test is flooding the same host is a session whose first
        // read is already queued behind that flood, and the wait below cannot tell that
        // apart from a shell that is not reading its input at all.
        let _alone = support::exclusive();
        let Some(session) = shell_for_the_flood() else {
            support::report(
                "local_interrupt",
                "SKIP the_shell_forwards_the_interrupt_to_the_program_it_started: no shell to \
                 start the flood from",
            );
            return;
        };
        forwards_the_interrupt(
            session,
            "the_shell_forwards_the_interrupt_to_the_program_it_started",
        );
    }

    /// The same measurement on the shell a local terminal starts by default.
    ///
    /// On Windows that is Windows PowerShell, not the `cmd.exe` the test above uses; the
    /// factory path is exactly where the interrupt went unmeasured.
    #[test]
    fn the_default_shell_forwards_the_interrupt_to_the_program_it_started() {
        let _alone = support::exclusive();
        let Some(session) = Session::start() else {
            support::report(
                "local_interrupt",
                "SKIP the_default_shell_forwards_the_interrupt_to_the_program_it_started: no \
                 shell on this machine",
            );
            return;
        };
        #[cfg(windows)]
        assert!(
            session
                .shell
                .program()
                .to_ascii_lowercase()
                .contains("windowspowershell"),
            "the shell a local terminal starts on Windows is Windows PowerShell, got {}",
            session.shell.program()
        );
        forwards_the_interrupt(
            session,
            "the_default_shell_forwards_the_interrupt_to_the_program_it_started",
        );
    }

    /// Starts the flood in `session`, presses Ctrl+C and reports what it measured.
    ///
    /// Shared so the factory shell is measured by exactly the same rules as `cmd.exe`;
    /// `label` is the test the failure or skip names, so either ending stays traceable.
    /// The caller holds [support::exclusive]: this measures what it can see, and a
    /// second flooding session on the same console host would make every reading here a
    /// reading of the scheduler.
    fn forwards_the_interrupt(mut session: Session, label: &str) {
        wait_until_it_takes_input(&mut session);
        wait_until_the_shell_is_quiet(&mut session);
        start_the_flood(&mut session, label);

        match support::interrupt_flood(&mut session) {
            Outcome::Stopped {
                bytes_at_press,
                stopped_after,
                presses,
            } => support::report(
                "local_interrupt",
                &format!(
                    "shell path: the flood started from a shell printed {} bytes, saw the \
                     Ctrl+C on press {presses} of at most {}, and stopped after \
                     {stopped_after:?}",
                    bytes_at_press,
                    support::PRESS_LIMIT
                ),
            ),
            Outcome::StillPrinting {
                bytes_at_press,
                bytes_now,
                presses,
            } => panic!(
                "the program started from a shell saw the Ctrl+C and kept printing: {} bytes \
                 at press {presses}, {} bytes now, no {:?} of silence within {:?}",
                bytes_at_press,
                bytes_now,
                support::STOP_WINDOW * support::STOP_WINDOWS,
                support::STOP_BUDGET
            ),
            Outcome::Undeliverable {
                presses,
                bytes_at_first_press,
                bytes_now,
            } => {
                support::undelivered(
                    "local_interrupt",
                    label,
                    presses,
                    bytes_at_first_press,
                    bytes_now,
                    "R8's shell path is unmeasured here, not passing.",
                );
                return;
            }
        }

        // The program is gone and the shell is back: R8 asks for the session to survive
        // the interrupt, and that is only true if the shell takes the next command.
        shell_takes_a_command_after_the_interrupt(&mut session);
    }

    /// Writes the command that starts the flood, and proves by its own output that it ran.
    ///
    /// The command has to be running before the byte is sent, otherwise the press
    /// interrupts the prompt instead of the program - and a console that is busy can lose
    /// the whole line, the way it was measured to lose the control byte itself. So a write
    /// that produces no flood output is written again, at most [LAUNCH_ATTEMPTS] times,
    /// and what "started" means does not move: at least [support::OUTPUT_BEFORE_INTERRUPT]
    /// new bytes and a real tick line from the program. The window is short because the
    /// flood prints every 5 ms; it is a bound on how long a lost line is given, not a
    /// wider wait for a slow machine.
    fn start_the_flood(session: &mut Session, label: &str) {
        let command = flood_command_for(&session.shell);
        for attempt in 1..=LAUNCH_ATTEMPTS {
            let before = session.bytes_read();
            session.run(&command);
            let started = Instant::now();
            loop {
                session.pump();
                let text = session.core.snapshot().text().join("\n");
                if session.bytes_read() >= before + support::OUTPUT_BEFORE_INTERRUPT
                    && text.contains("tick ")
                {
                    return;
                }
                if started.elapsed() >= LAUNCH_OBSERVE {
                    support::report(
                        label,
                        &format!(
                            "the launch command produced no flood output in {LAUNCH_OBSERVE:?} \
                             (attempt {attempt} of at most {LAUNCH_ATTEMPTS}); writing it again"
                        ),
                    );
                    break;
                }
                std::thread::sleep(support::READ_INTERVAL);
            }
        }
        let text = session.core.snapshot().text().join("\n");
        panic!(
            "the flood never started from a shell prompt: {} bytes came back after \
             {LAUNCH_ATTEMPTS} writes of the command; the screen held:\n{text}",
            session.bytes_read()
        );
    }

    /// The negative control: a flood that cannot be stopped must never look stopped.
    ///
    /// The same program with a Ctrl+C handler that reports the marker and keeps printing,
    /// interrupted the same way. The output has to keep arriving, so the detection above
    /// cannot be satisfied by a quiet moment instead of a real stop. If this test ever
    /// passes the assertion below, the interrupt test above proves nothing.
    #[test]
    fn a_flood_that_ignores_the_interrupt_never_looks_stopped() {
        let _alone = support::exclusive();
        let Some(mut session) = Session::start_program(FLOOD_PROGRAM, &["--ignore-interrupt"])
        else {
            support::report(
                "local_interrupt",
                "SKIP a_flood_that_ignores_the_interrupt_never_looks_stopped: the flood \
                 program is not available",
            );
            return;
        };

        match support::interrupt_flood(&mut session) {
            Outcome::StillPrinting {
                bytes_at_press,
                bytes_now,
                presses,
            } => support::report(
                "local_interrupt",
                &format!(
                    "negative control: the flood saw the Ctrl+C on press {presses} of at most \
                     {} and kept printing - {bytes_at_press} bytes at the press, {bytes_now} \
                     now - so the stop the test above reads is not a quiet moment",
                    support::PRESS_LIMIT
                ),
            ),
            Outcome::Stopped {
                bytes_at_press,
                stopped_after,
                ..
            } => panic!(
                "a flood that ignores Ctrl+C went quiet {stopped_after:?} after the press \
                 ({bytes_at_press} bytes at it), which is the silence the interrupt test reads \
                 as a stop: that detection would have passed for a program that never stopped"
            ),
            Outcome::Undeliverable {
                presses,
                bytes_at_first_press,
                bytes_now,
            } => {
                support::undelivered(
                    "local_interrupt",
                    "a_flood_that_ignores_the_interrupt_never_looks_stopped",
                    presses,
                    bytes_at_first_press,
                    bytes_now,
                    "The negative control did not run, so nothing is proven about the stop \
                     detection here.",
                );
            }
        }
    }
}
