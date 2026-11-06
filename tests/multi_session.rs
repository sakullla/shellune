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
/// prints one line every 5 ms, which is a stream a terminal can process input against.
/// The loop that used to be typed into `cmd.exe` printed ~600 KB/s, and that rate
/// starved the pseudo console's input so completely that nine Ctrl+C presses over 24 s
/// were never processed - no test can measure an interrupt the console never handles.
/// The program is built for tests only (`required-features = ["test-support"]`), and this
/// module is behind the same feature.
#[cfg(feature = "test-support")]
mod interrupt {
    use super::*;

    /// The flood program, as Cargo built it for this test binary.
    const FLOOD_PROGRAM: &str = env!("CARGO_BIN_EXE_shellune-flood");

    /// How long the flooded session must stay silent before its output counts as stopped,
    /// and how many reads must have seen that silence.
    ///
    /// The counter, not the screen, is what is measured: a full grid and a stopped grid
    /// look the same, so the only honest stop is output that really stopped arriving.
    const STOP_WINDOW: Duration = Duration::from_secs(1);
    const STOP_WINDOW_READS: u32 = 40;

    /// How many of those windows the silence must last before it counts as a stop.
    ///
    /// One window is not proof on a busy machine: a console host that is not being
    /// scheduled holds its output back, which looks exactly like a stop and is not one.
    /// Two windows cost one more second and are not a wider window: the window is still
    /// one second, measured twice without a byte in between.
    const STOP_WINDOWS: u32 = 2;

    /// How long the flood is given to produce its first bytes.
    ///
    /// Generous because a busy machine slows the console host rather than the program:
    /// with a cold cargo build running, one measured run of the old loop printed only 1791
    /// bytes in thirty seconds. The stop window is a different number and is never
    /// widened.
    const FLOOD_BUDGET: Duration = Duration::from_secs(90);

    /// The most time the interrupt is given before the test says it did not stop.
    ///
    /// The stop window above is not widened for a slow machine; this is only the point at
    /// which a session that is still printing is declared uninterruptible. Generous
    /// because a console host under a cold cargo build is not scheduled for long
    /// stretches - a line typed into `cmd.exe` under one sat unread for thirty seconds -
    /// so the press can wait. The program prints every 5 ms and installs no handler, so
    /// the press itself is not the slow part.
    const INTERRUPT_BUDGET: Duration = Duration::from_secs(35);

    #[test]
    fn interrupting_one_session_leaves_the_other_responsive() {
        // Two sessions, two processes: the flood in its own PTY, and a shell that has to
        // keep answering while the flood is interrupted. The byte goes through the same
        // PTY write the application's Ctrl+C uses.
        let (Some(mut flood), Some(mut other)) =
            (Rig::start_program(FLOOD_PROGRAM, &[]), Rig::start())
        else {
            eprintln!("no flood program or shell on this machine, skipping");
            return;
        };

        let started = Instant::now();
        // The program is endless, so it cannot end by itself and make the test pass
        // without an interrupt; these bytes only prove it is mid-stream.
        while flood.bytes < 2048 {
            flood.pump();
            assert!(
                started.elapsed() < FLOOD_BUDGET,
                "the flood never started: {} bytes came back",
                flood.bytes
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // One Ctrl+C, and this session's output has to stop. One press is what a user
        // does, and the flood prints every 5 ms with no handler installed, so the console
        // has the input queue to itself between two lines; pressing again would hide a
        // stream too dense to interrupt instead of reporting it.
        let pressed = Instant::now();
        flood.write(&[0x03]);

        let mut last = flood.bytes;
        let mut last_growth = pressed;
        let mut quiet_reads = 0;
        let stopped_after = loop {
            flood.pump();
            let now = Instant::now();
            if flood.bytes != last {
                last = flood.bytes;
                last_growth = now;
                quiet_reads = 0;
            } else {
                quiet_reads += 1;
            }
            if last_growth.elapsed() >= STOP_WINDOW * STOP_WINDOWS
                && quiet_reads >= STOP_WINDOW_READS * STOP_WINDOWS
            {
                break last_growth.duration_since(pressed);
            }
            assert!(
                pressed.elapsed() < INTERRUPT_BUDGET,
                "the flooded session kept printing {:?} after one Ctrl+C, {} bytes so far",
                pressed.elapsed(),
                flood.bytes
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        eprintln!(
            "multi_session: output stopped {stopped_after:?} after the press, {} bytes at \
             the press",
            last
        );

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
