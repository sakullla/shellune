//! The shell, its PTY and the terminal that parses what it writes.
//!
//! Two integration binaries drive a real shell: the terminal tests in local_shell.rs and
//! the interrupt tests in local_interrupt.rs (R8). The rig lives here once, so both read
//! the same bytes through the same code instead of through two copies that can drift
//! apart.
//!
//! # Why the interrupt tests are a binary of their own
//!
//! Measured on this machine: the four Ctrl+C cases print "this runner did not deliver
//! the control byte" when the rest of the terminal tests have already run in the same
//! process, and all four pass - marker on the first press - when they are the only
//! sessions that process has created. Nothing in the shell or in the product differs
//! between those two runs; a process that has already created and closed a dozen pseudo
//! consoles is where this runner stops delivering the byte. A separate binary is
//! therefore the only way to measure the interrupt instead of skipping it, and a skip
//! that looks green is exactly what R6 forbids.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use shellune_core::LocalShell;
use shellune_terminal::{
    ExitReport, GridSize, LocalSession, PtyEvent, Snapshot, TerminalCore, encode_text,
    platform_default, program_exists,
};

/// A shell, its PTY and the terminal that parses what it writes.
pub struct Session {
    pub shell: LocalShell,
    pub pty: LocalSession,
    pub core: TerminalCore,
    pub exited: Option<ExitReport>,
    /// Bytes the shell has produced since it started.
    ///
    /// This is the only monotonic progress signal a test can rely on: the visible
    /// grid has 30 rows and saturates, so "the screen stopped changing" is also
    /// what a screen that is merely full looks like.
    pub bytes_read: u64,
}

impl Session {
    /// Starts the machine's default shell at a size a shell can work in.
    pub fn start() -> Option<Self> {
        Self::start_shell(platform_default())
    }

    /// Starts a given shell through the same entry point the application uses.
    ///
    /// The application's path is the interesting one for a login shell: it is where
    /// the login flag and the environment are added (R14).
    pub fn start_shell(shell: LocalShell) -> Option<Self> {
        if !program_exists(shell.program()) {
            return None;
        }
        let grid = GridSize::new(100, 30);
        let pty = LocalSession::spawn(&shell, grid, None).ok()?;
        Some(Self {
            shell,
            pty,
            core: TerminalCore::new(grid.cols, grid.rows, 2000),
            exited: None,
            bytes_read: 0,
        })
    }

    pub fn start_program(program: &str, args: &[&str]) -> Option<Self> {
        if !program_exists(program) {
            return None;
        }
        let mut command = portable_pty::CommandBuilder::new(program);
        command.args(args);
        let grid = GridSize::new(100, 30);
        let pty = LocalSession::spawn_command(command, grid, program).ok()?;
        Some(Self {
            shell: LocalShell::new(program, program),
            pty,
            core: TerminalCore::new(grid.cols, grid.rows, 2000),
            exited: None,
            bytes_read: 0,
        })
    }

    /// Types text and presses Return.
    pub fn run(&mut self, line: &str) {
        let mut bytes = encode_text(line);
        bytes.push(b'\r');
        self.write(&bytes);
    }

    pub fn write(&mut self, bytes: &[u8]) {
        self.pty.write(bytes).expect("the shell accepts input");
    }

    /// Reads what the shell wrote into the terminal core.
    ///
    /// Returns how many bytes arrived, and adds them to the session total.
    pub fn pump(&mut self) -> usize {
        let mut read = 0;
        for event in self.pty.drain() {
            match event {
                PtyEvent::Data(bytes) => {
                    read += bytes.len();
                    let effects = self.core.feed(&bytes);
                    if !effects.writes.is_empty() {
                        // Answering the terminal's own queries is part of being a
                        // terminal: a shell that asked for a reply waits for it.
                        let _ = self.pty.write(&effects.writes);
                    }
                }
                PtyEvent::Exited(report) => self.exited = Some(report),
                PtyEvent::ReadFailed(reason) => panic!("the PTY failed: {reason}"),
            }
        }
        self.bytes_read += read as u64;
        read
    }

    /// Every byte the shell has produced so far.
    ///
    /// Only the interrupt tests measure bytes, so the method exists with them.
    #[cfg(feature = "test-support")]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Waits until the visible screen satisfies a condition.
    pub fn wait_for(
        &mut self,
        what: &str,
        mut predicate: impl FnMut(&Snapshot, &str) -> bool,
    ) -> String {
        self.poll_until(what, |_session, snapshot, text| predicate(snapshot, text))
    }

    /// Polls the PTY until `step` says the screen is ready, then returns it.
    ///
    /// `step` gets the session as well as the screen, so a test can react to what
    /// it read — send another key, or offer a line again — instead of sleeping a
    /// guessed amount and hoping the shell was ready for it.
    pub fn poll_until(
        &mut self,
        what: &str,
        mut step: impl FnMut(&mut Self, &Snapshot, &str) -> bool,
    ) -> String {
        // A last-resort deadline so a broken terminal fails instead of hanging; the
        // condition, not this number, decides when the wait is over.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.pump();
            let snapshot = self.core.snapshot();
            let text = snapshot.text().join("\n");
            if step(self, &snapshot, &text) {
                return text;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}; the screen held:\n{text}\n---\n{}",
                    self.shell.name()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn screen(&mut self) -> (Snapshot, String) {
        self.pump();
        let snapshot = self.core.snapshot();
        let text = snapshot.text().join("\n");
        (snapshot, text)
    }

    pub fn close(&mut self) {
        self.pty.close();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn contains(needle: &str) -> impl FnMut(&Snapshot, &str) -> bool + '_ {
    move |_snapshot, text| text.contains(needle)
}
