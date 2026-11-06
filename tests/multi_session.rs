//! Several terminals at once, each with its own process (R7).
//!
//! Every test here starts real shell processes in real PTYs and reads what they
//! wrote through the same terminal core the application uses. What is being measured
//! is the fault isolation R7 asks for: one terminal being closed, interrupted or
//! exited must not change what the others do.
//!
//! The window-side half of R7 - tabs, switching, the reconnect button and the
//! navigation that stays usable while a connection is pending - cannot be observed
//! without a window and is covered by the GPUI tests in the application crate.

use std::time::{Duration, Instant};

use shellune_core::{EventTag, SessionId};
use shellune_terminal::{
    ExitReport, GridSize, LocalSession, PtyEvent, Snapshot, TerminalCore, encode_text,
    platform_default, program_exists,
};

#[cfg(feature = "test-support")]
mod support;

/// One shell, its PTY and the terminal that parses what it writes.
///
/// The tag is what the window uses to route an event: it names one session and one
/// process of it, so two rigs with different tags are two different sessions even when
/// they run the same program.
struct Rig {
    tag: EventTag,
    shell_name: String,
    pty: LocalSession,
    core: TerminalCore,
    exited: Option<ExitReport>,
    bytes: u64,
}

impl Rig {
    /// A shell of this machine in its own PTY.
    fn start() -> Option<Self> {
        let shell = platform_default();
        if !program_exists(shell.program()) {
            return None;
        }
        let grid = GridSize::new(100, 30);
        let pty = LocalSession::spawn(&shell, grid, None).ok()?;
        Some(Self::wrap(shell.name(), pty, grid))
    }

    /// A named program in its own PTY, for the tests that need a known one.
    ///
    /// Only the interrupt test starts a program of its own, so this exists with it.
    #[cfg(feature = "test-support")]
    fn start_program(program: &str, args: &[&str]) -> Option<Self> {
        if !program_exists(program) {
            return None;
        }
        let mut command = portable_pty::CommandBuilder::new(program);
        command.args(args);
        let grid = GridSize::new(100, 30);
        let pty = LocalSession::spawn_command(command, grid, program).ok()?;
        Some(Self::wrap(program, pty, grid))
    }

    fn wrap(name: &str, pty: LocalSession, grid: GridSize) -> Self {
        Self {
            tag: EventTag::new(SessionId::new(), 0),
            shell_name: name.to_owned(),
            pty,
            core: TerminalCore::new(grid.cols, grid.rows, 2000),
            exited: None,
            bytes: 0,
        }
    }

    fn run(&mut self, line: &str) {
        let mut bytes = encode_text(line);
        bytes.push(b'\r');
        self.pty.write(&bytes).expect("the shell takes input");
    }

    /// Writes bytes to the session, for the interrupt the test sends.
    #[cfg(feature = "test-support")]
    fn write(&mut self, bytes: &[u8]) {
        self.pty.write(bytes).expect("the shell takes input");
    }

    /// Applies whatever the shell wrote, and counts the bytes.
    fn pump(&mut self) -> usize {
        let mut read = 0;
        for event in self.pty.drain() {
            match event {
                PtyEvent::Data(bytes) => {
                    read += bytes.len();
                    let effects = self.core.feed(&bytes);
                    if !effects.writes.is_empty() {
                        let _ = self.pty.write(&effects.writes);
                    }
                }
                PtyEvent::Exited(report) => self.exited = Some(report),
                PtyEvent::ReadFailed(reason) => panic!("{} failed: {reason}", self.shell_name),
            }
        }
        self.bytes += read as u64;
        read
    }

    fn screen(&mut self) -> (Snapshot, String) {
        self.pump();
        let snapshot = self.core.snapshot();
        let text = snapshot.text().join("\n");
        (snapshot, text)
    }

    /// Waits until the marker is on screen often enough to prove the program printed
    /// it, rather than the terminal echoing the command line that contains it.
    fn wait_for_marker(&mut self, marker: &str, what: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (_, text) = self.screen();
            if text.matches(marker).count() >= 2 {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "{} never printed {marker} ({what}); the screen held:\n{text}",
                self.shell_name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.pty.close();
    }
}

/// The command that prints one marker.
fn print_marker(marker: &str) -> String {
    if cfg!(windows) {
        format!("Write-Output ('{marker}')")
    } else {
        format!("echo '{marker}'")
    }
}

#[test]
fn several_shells_at_once_keep_their_output_to_themselves() {
    let mut rigs = Vec::new();
    for _ in 0..3 {
        match Rig::start() {
            Some(rig) => rigs.push(rig),
            None => {
                eprintln!("no shell on this machine, skipping");
                return;
            }
        }
    }
    let mut ids: Vec<SessionId> = rigs.iter().map(|rig| rig.tag.session).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "each terminal must have its own session");

    for (index, rig) in rigs.iter_mut().enumerate() {
        rig.run(&print_marker(&format!("marker-{index}-only")));
    }
    let mut screens = Vec::new();
    for (index, rig) in rigs.iter_mut().enumerate() {
        let marker = format!("marker-{index}-only");
        let text = rig.wait_for_marker(&marker, "its own marker");
        screens.push((marker, text));
    }

    // Each screen shows its own output and nothing from the other sessions.
    for (index, (marker, text)) in screens.iter().enumerate() {
        assert!(
            text.contains(marker),
            "session {index} lost its own output:\n{text}"
        );
        for (other, (foreign, _)) in screens.iter().enumerate() {
            if other != index {
                assert!(
                    !text.contains(foreign),
                    "session {index} shows the output of session {other}:\n{text}"
                );
            }
        }
    }
}

#[test]
fn closing_one_shell_leaves_the_others_answerable() {
    let (Some(mut first), Some(mut second)) = (Rig::start(), Rig::start()) else {
        eprintln!("no shell on this machine, skipping");
        return;
    };
    first.run(&print_marker("before-close"));
    first.wait_for_marker("before-close", "its marker");

    // Closing one session ends its process, and only its process.
    first.pty.close();
    assert!(first.pty.is_closed());
    assert!(
        !second.pty.is_closed(),
        "closing one session must not close another one"
    );

    second.run(&print_marker("still-alive"));
    let text = second.wait_for_marker("still-alive", "its marker after the other closed");
    assert!(
        text.contains("still-alive"),
        "the surviving session stopped answering:\n{text}"
    );
}

#[test]
fn a_shell_that_exits_reports_only_its_own_end() {
    let (Some(mut first), Some(mut second)) = (Rig::start(), Rig::start()) else {
        eprintln!("no shell on this machine, skipping");
        return;
    };
    first.run("exit 3");

    let deadline = Instant::now() + Duration::from_secs(30);
    while first.exited.is_none() {
        first.pump();
        assert!(Instant::now() < deadline, "the exit was never reported");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(first.exited, Some(ExitReport::Code { code: 3 }));

    // The other session did not notice: it still runs and still answers.
    second.run(&print_marker("after-the-exit"));
    let text = second.wait_for_marker("after-the-exit", "its marker after the other exited");
    assert!(text.contains("after-the-exit"), "{text}");
    assert!(
        second.exited.is_none(),
        "the other session must still be running"
    );
}

/// Interrupting one session while another keeps running (R7).
///
/// The flood is `shellune-flood`, the program the local-shell interrupt tests use: it
/// prints one line every 5 ms, a stream a terminal can process input against, and it prints
/// `flood: interrupted` when its Ctrl+C handler runs. The loop that used to be typed
/// into `cmd.exe` printed ~600 KB/s, and that rate starved the pseudo console's input
/// so completely that nine Ctrl+C presses over 24 s were never processed - no test can
/// measure an interrupt the console never handles.
///
/// The marker is what makes the result mean something: this runner was measured to drop the
/// byte (`cargo test` failed 6 of 8 runs of the identical test binary, which passes 4 of
/// 4 when run directly), and without the marker a stream that never stops cannot be told
/// apart from output that was never told to stop. A press that produces no marker at all is
/// reported as this runner not delivering the byte, not as a pass.
#[cfg(feature = "test-support")]
mod interrupt {
    use super::*;
    use crate::support::{self, Flood, Outcome, Sample};

    /// The flood program, as Cargo built it for this test binary.
    const FLOOD_PROGRAM: &str = env!("CARGO_BIN_EXE_shellune-flood");

    /// The flood's session, read as bytes and text, with one Ctrl+C per press.
    ///
    /// The byte is written into this session's PTY, which is the same call the
    /// application's Ctrl+C makes, and the other session keeps its own process.
    impl Flood for Rig {
        fn sample(&mut self) -> Sample {
            self.pump();
            Sample {
                bytes: self.bytes,
                text: self.core.snapshot().text().join("\n"),
            }
        }

        fn press(&mut self) {
            self.write(&[0x03]);
        }
    }

    #[test]
    fn interrupting_one_session_leaves_the_other_responsive() {
        let _alone = support::exclusive();
        // Two sessions, two processes: the flood in its own PTY, and a shell that has to
        // keep answering while the flood is interrupted.
        let (Some(mut flood), Some(mut other)) =
            (Rig::start_program(FLOOD_PROGRAM, &[]), Rig::start())
        else {
            support::report(
                "multi_session",
                "SKIP interrupting_one_session_leaves_the_other_responsive: no flood program \
                 or shell on this machine",
            );
            return;
        };

        match support::interrupt_flood(&mut flood) {
            Outcome::Stopped {
                bytes_at_press,
                stopped_after,
                presses,
            } => support::report(
                "multi_session",
                &format!(
                    "the flooded session printed {} bytes, saw the Ctrl+C on press {presses} of \
                     at most {}, and stopped after {stopped_after:?}",
                    bytes_at_press,
                    support::PRESS_LIMIT
                ),
            ),
            Outcome::StillPrinting {
                bytes_at_press,
                bytes_now,
                presses,
            } => panic!(
                "the flooded session saw the Ctrl+C and kept printing: {bytes_at_press} bytes \
                 at press {presses}, {bytes_now} bytes now, no {:?} of silence within {:?}",
                support::STOP_WINDOW * support::STOP_WINDOWS,
                support::STOP_BUDGET
            ),
            Outcome::Undeliverable {
                presses,
                bytes_at_first_press,
                bytes_now,
            } => {
                support::undelivered(
                    "multi_session",
                    "interrupting_one_session_leaves_the_other_responsive",
                    presses,
                    bytes_at_first_press,
                    bytes_now,
                    "The isolation of an interrupt is unmeasured here, not passing.",
                );
                return;
            }
        }

        // The interrupted session is the only one that changed: the other one answers a
        // command typed after the interrupt.
        other.run(&print_marker("other-session"));
        let text = other.wait_for_marker("other-session", "its marker after the interrupt");
        assert!(
            text.contains("other-session"),
            "the other session stopped responding when one was interrupted:\n{text}"
        );
    }
}

#[test]
fn a_tag_names_one_session_and_one_process_of_it() {
    let session = SessionId::new();
    let first = EventTag::new(session, 0);
    let second = EventTag::new(session, 1);
    let other = EventTag::new(SessionId::new(), 0);

    assert_ne!(first, second, "a reconnect is a different process");
    assert_ne!(first, other, "two sessions are never the same target");
    assert_eq!(first.session, second.session, "the tab keeps its identity");
}
