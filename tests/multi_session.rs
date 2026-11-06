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

    /// Waits until the shell has answered a typed line.
    ///
    /// A prompt on screen proves the shell is drawing; it does not prove it is reading
    /// what arrives, and a line written before it reads is dropped without a trace.
    /// The marker is offered again on a fixed interval until the shell prints it back,
    /// which is the only evidence that input really reaches the program.
    fn wait_until_it_takes_input(&mut self) {
        let deadline = Instant::now() + FLOOD_BUDGET;
        let mut next_offer = Instant::now();
        loop {
            let (_, text) = self.screen();
            if text.matches(INPUT_MARKER).count() >= 2 {
                return;
            }
            if Instant::now() >= next_offer {
                self.run(&format!("echo {INPUT_MARKER}"));
                next_offer = Instant::now() + Duration::from_millis(500);
            }
            assert!(
                Instant::now() < deadline,
                "{} never answered a typed line, so no command could reach it",
                self.shell_name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The marker typed to find out whether a shell is reading input yet.
const INPUT_MARKER: &str = "shellune-input-check";

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

/// How often the same Ctrl+C is offered again while the flooded session still prints.
///
/// One press is not enough to rely on: a single 0x03 written into a flooding cmd.exe
/// was measured to have no effect at all on a reviewer's machine, where the loop kept
/// printing to ~12.8 MB for the whole 20 s budget. A press lost that way never comes
/// back on its own, so the same byte is written again on this interval until the
/// output stops — and a session that ignores every press still fails the test.
///
/// Longer than the quiet this test has to see before it believes the stop: a press that
/// reaches a session which already stopped makes it print another ^C and prompt, and
/// that output restarts the silence being measured. A press interval equal to that
/// silence keeps cutting it short, and the test then presses for ever on a loop that
/// stopped after its first press.
const PRESS_INTERVAL: Duration = Duration::from_secs(3);

/// The most presses one interrupt may use, so the retries stay inside the budget.
///
/// Nine presses three seconds apart reach twenty-four seconds, which leaves the
/// silence after the last one inside the [INTERRUPT_BUDGET].
const MAX_PRESSES: u32 = 9;

/// How long the flood is given to produce its first bytes.
///
/// Generous because a busy machine slows the console host rather than the loop: with a
/// cold cargo build running, one measured run of this loop printed only 1791 bytes in
/// thirty seconds. The stop window is a different number and is never widened.
const FLOOD_BUDGET: Duration = Duration::from_secs(90);

/// The most time the interrupt is given before the test says it did not stop.
///
/// The stop window above is not widened for a slow machine; this is only the point at
/// which a session that is still printing is declared uninterruptible. It is generous
/// because presses queue in the console while the machine is busy: with a concurrent
/// cargo build running, one measured run stopped 18.8 s after the first press, nearly
/// five seconds after the last press was written.
const INTERRUPT_BUDGET: Duration = Duration::from_secs(35);

#[test]
fn interrupting_one_session_leaves_the_other_responsive() {
    // The flood runs cmd.exe on Windows and the platform shell elsewhere: a console
    // program the interrupt is meant for, with the byte written through the same PTY
    // the application uses. How promptly it lands is measured below rather than
    // claimed here - see the press-to-stop report this test prints.
    let flood = if cfg!(windows) {
        Rig::start_program("cmd.exe", &["/q"])
    } else {
        Rig::start()
    };
    let (Some(mut flood), Some(mut other)) = (flood, Rig::start()) else {
        eprintln!("no interruptible shell on this machine, skipping");
        return;
    };
    // A line typed into a console that is not reading yet is dropped, and the flood
    // then never begins. Drawing a prompt does not prove the shell is reading: with a
    // cold cargo build running, cmd.exe drew its prompt and then consumed nothing for
    // thirty seconds. So a line is typed first and awaited, and only a shell that
    // answered gets the loop.
    flood.wait_until_it_takes_input();
    if cfg!(windows) {
        flood.run("for /l %i in (1,1,1000000000) do @echo tick");
    } else {
        flood.run("while true; do echo tick; sleep 0.01; done");
    }

    let started = Instant::now();
    // The line is typed once and the budget waits for it. It is deliberately not typed
    // again: a line the console has not read yet still sits in its input buffer and runs
    // later, so a second copy would start a second loop behind the first one.
    while flood.bytes < 2048 {
        flood.pump();
        assert!(
            started.elapsed() < FLOOD_BUDGET,
            "the flood never started: {} bytes came back after the loop was typed",
            flood.bytes
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Ctrl+C, and this session's output has to stop. The byte is offered again on a
    // fixed interval because a press the console dropped cannot be told apart from
    // one it has not acted on yet, and only pressing again resolves that.
    let pressed = Instant::now();
    let mut next_press = pressed;
    let mut presses: u32 = 0;
    let mut last = flood.bytes;
    let mut last_growth = pressed;
    let mut quiet_reads = 0;
    let (stopped_after, presses) = loop {
        flood.pump();
        let now = Instant::now();
        if now >= next_press && presses < MAX_PRESSES {
            flood.write(&[0x03]);
            presses += 1;
            next_press = now + PRESS_INTERVAL;
        }
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
            break (last_growth.duration_since(pressed), presses);
        }
        assert!(
            pressed.elapsed() < INTERRUPT_BUDGET,
            "the flooded session kept printing {:?} after {presses} Ctrl+C press(es), \
             {} bytes so far",
            pressed.elapsed(),
            flood.bytes
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    eprintln!(
        "multi_session: output stopped {stopped_after:?} after the first press, \
         {presses} press(es) used, {} bytes at the press",
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
