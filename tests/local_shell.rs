//! Integration tests against a real shell process (R2, R8).
//!
//! These tests start the shell this machine actually has, feed it commands and read
//! what comes back through the same PTY and the same terminal core the application
//! uses. Nothing is stubbed: a passing result here means a real program ran, a real
//! byte stream was parsed and real text landed on the grid.
//!
//! What cannot be checked here is what needs a window: drawing, the input method,
//! clipboard and mouse. Those are covered by the GPUI tests in the application and
//! are listed in the task record, so this file never claims to have verified them.

use std::time::{Duration, Instant};

use shellune_core::LocalShell;
use shellune_terminal::{
    ExitReport, GridSize, LocalSession, PtyEvent, Snapshot, TermMode, TerminalCore, encode_text,
    palette, platform_default, program_exists,
};

/// A shell, its PTY and the terminal that parses what it writes.
struct Session {
    shell: LocalShell,
    pty: LocalSession,
    core: TerminalCore,
    exited: Option<ExitReport>,
}

impl Session {
    /// Starts the machine's default shell at a size a shell can work in.
    fn start() -> Option<Self> {
        let shell = platform_default();
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
        })
    }

    fn start_program(program: &str, args: &[&str]) -> Option<Self> {
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
        })
    }

    /// Types text and presses Return.
    fn run(&mut self, line: &str) {
        let mut bytes = encode_text(line);
        bytes.push(b'\r');
        self.write(&bytes);
    }

    fn write(&mut self, bytes: &[u8]) {
        self.pty.write(bytes).expect("the shell accepts input");
    }

    /// Reads what the shell wrote into the terminal core.
    fn pump(&mut self) {
        for event in self.pty.drain() {
            match event {
                PtyEvent::Data(bytes) => {
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
    }

    /// Waits until the visible screen satisfies a condition.
    fn wait_for(
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
    fn poll_until(
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

    fn screen(&mut self) -> (Snapshot, String) {
        self.pump();
        let snapshot = self.core.snapshot();
        let text = snapshot.text().join("\n");
        (snapshot, text)
    }

    fn close(&mut self) {
        self.pty.close();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

fn contains(needle: &str) -> impl FnMut(&Snapshot, &str) -> bool + '_ {
    move |_snapshot, text| text.contains(needle)
}

#[test]
fn a_real_shell_runs_a_command_and_echoes_its_output() {
    let Some(mut session) = Session::start() else {
        eprintln!("no shell on this machine, skipping");
        return;
    };

    if cfg!(windows) {
        session.run("Write-Output 'hello-from-shellune'");
    } else {
        session.run("echo hello-from-shellune");
    }
    let screen = session.wait_for("the echo output", contains("hello-from-shellune"));
    assert!(
        screen.contains("hello-from-shellune"),
        "the shell output did not reach the grid:\n{screen}"
    );
}

#[test]
fn changing_directory_shows_the_new_path() {
    let Some(mut session) = Session::start() else {
        return;
    };
    // Not canonicalised: on Windows that yields an extended-length path
    // (\?C:...) that the shell never prints.
    let expected = std::env::temp_dir()
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_owned();

    if cfg!(windows) {
        session.run("Set-Location $env:TEMP; (Get-Location).Path");
    } else {
        session.run("cd \"$TMPDIR\" 2>/dev/null || cd /tmp; pwd");
    }
    let screen = session.wait_for("the new directory", |_snapshot, text| {
        text.contains(expected.as_str()) || text.contains(&expected.replace('\\', "/"))
    });
    assert!(
        screen.contains(&expected) || screen.contains(&expected.replace('\\', "/")),
        "the new directory was not reported: expected {expected} in\n{screen}"
    );
}

#[test]
fn colour_output_reaches_the_grid_as_colour() {
    let Some(mut session) = Session::start() else {
        return;
    };

    if cfg!(windows) {
        // PowerShell: emit a real SGR green sequence without the shell interpreting
        // it first.
        session.run(r#"Write-Host "$([char]27)[32mcolour-check$([char]27)[0m""#);
    } else {
        session.run(r#"printf '\033[32mcolour-check\033[0m\n'"#);
    }
    // The command line itself contains the marker, so wait until the screen shows
    // it twice: once echoed, once as output.
    session.wait_for("the coloured output", |_snapshot, text| {
        text.matches("colour-check").count() >= 2
    });

    let (snapshot, text) = session.screen();
    let green = palette::ANSI[2];
    let row = snapshot
        .rows
        .iter()
        .rev()
        .find(|row| {
            let line: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
            line.contains("colour-check")
        })
        .expect("the output row is on screen");
    let line: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
    let start = line.find("colour-check").expect("the marker is in the row");
    let coloured = row
        .cells
        .iter()
        .skip(start)
        .take("colour-check".len())
        .all(|cell| cell.foreground == green);
    assert!(
        coloured,
        "the green SGR sequence did not colour the output cells:\n{text}"
    );
}

#[test]
fn chinese_text_survives_the_round_trip() {
    let Some(mut session) = Session::start() else {
        return;
    };

    if cfg!(windows) {
        session.run("Write-Output '中文测试'");
    } else {
        session.run("echo 中文测试");
    }
    let screen = session.wait_for("the Chinese output", contains("中文测试"));
    assert!(
        screen.contains("中文测试"),
        "Chinese text did not survive the byte stream:\n{screen}"
    );
}

#[test]
fn chinese_input_reaches_the_shell() {
    let Some(mut session) = Session::start() else {
        return;
    };
    // Typed input, not output: this is what an input method commits.
    session.write("中文输入".as_bytes());
    // The shell echoes what it received; then clear the line so the test leaves no
    // half-typed command behind.
    session.wait_for("the echoed input", contains("中文输入"));
    session.write(&[0x03]);
}

#[test]
fn an_interactive_program_uses_the_alternate_screen_and_restores_the_shell() {
    // Vim is the reference interactive program: it switches to the alternate
    // screen, positions the cursor, colours text and reads keystrokes. Where it is
    // missing the test says so rather than pretending to have run it.
    let Some((session, run_vim)) = interactive_session() else {
        eprintln!("no Vim reachable from a shell here, skipping the alternate screen check");
        return;
    };
    let mut session = session;
    let shell_prompt = session
        .shell
        .program()
        .to_ascii_lowercase()
        .contains("bash")
        || session.shell.program().ends_with("/sh")
        || session.shell.program().ends_with("/zsh");
    if shell_prompt {
        session.run("echo before-vim");
        session.wait_for("the shell to print a marker", contains("before-vim"));
    }

    if !run_vim.is_empty() {
        session.run(&run_vim);
    }
    session.wait_for("vim to take the screen", |snapshot, _text| {
        snapshot.mode.contains(TermMode::ALT_SCREEN)
    });

    // A Vim that is really running accepts keystrokes and changes the screen.
    session.write(b"i");
    session.write("vim-was-here".as_bytes());
    let screen = session.wait_for("typed text in vim", contains("vim-was-here"));
    assert!(screen.contains("vim-was-here"), "{screen}");

    // Leaving Vim must give the screen back, not leave the alternate screen behind.
    session.write(&[0x1b]);
    session.write(b":q!\r");
    session.wait_for("vim to exit", |snapshot, _text| {
        !snapshot.mode.contains(TermMode::ALT_SCREEN)
    });
    if shell_prompt {
        let screen = session.wait_for("the shell screen again", contains("before-vim"));
        assert!(
            screen.contains("before-vim"),
            "the shell screen did not come back after Vim:\n{screen}"
        );
    }
}

/// A session that can start Vim, and the command that starts it.
///
/// On Windows the only Vim on this machine is the one Git installs, which a
/// PowerShell session cannot find because it is not on the Windows PATH. A POSIX
/// shell from the same installation can find it, so that is used when present; if
/// only Vim itself is available it is started directly.
fn interactive_session() -> Option<(Session, String)> {
    let grid = GridSize::new(100, 30);

    if !cfg!(windows) {
        if !program_exists("vim") {
            return None;
        }
        let shell = platform_default();
        let pty = LocalSession::spawn(&shell, grid, None).ok()?;
        let session = Session {
            shell,
            pty,
            core: TerminalCore::new(grid.cols, grid.rows, 2000),
            exited: None,
        };
        return Some((session, "vim -u NONE -N".to_owned()));
    }

    if let Some(bash) = git_install_path("bash") {
        let shell = LocalShell::new("Git Bash", bash);
        let pty = LocalSession::spawn(&shell, grid, None).ok()?;
        let session = Session {
            shell,
            pty,
            core: TerminalCore::new(grid.cols, grid.rows, 2000),
            exited: None,
        };
        return Some((session, "vim -u NONE -N".to_owned()));
    }

    let vim = git_install_path("vim")?;
    // Vim is the program itself here, so there is no command line to type.
    let session = Session::start_program(&vim, &["-u", "NONE", "-N"])?;
    Some((session, String::new()))
}

/// A program from the Git installation, which is not on the Windows PATH.
fn git_install_path(name: &str) -> Option<String> {
    let mut candidates = Vec::new();
    for root in ["C:\\Program Files\\Git", "C:\\Program Files (x86)\\Git"] {
        candidates.push(format!("{root}\\bin\\{name}.exe"));
        candidates.push(format!("{root}\\usr\\bin\\{name}.exe"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = std::path::Path::new(&local).join("Programs").join("Git");
        candidates.push(
            local
                .join("bin")
                .join(format!("{name}.exe"))
                .display()
                .to_string(),
        );
        candidates.push(
            local
                .join("usr")
                .join("bin")
                .join(format!("{name}.exe"))
                .display()
                .to_string(),
        );
    }
    candidates
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).is_file())
}

#[test]
fn a_shell_that_exits_reports_its_status() {
    let Some(mut session) = Session::start() else {
        return;
    };

    session.run("exit 4");
    let deadline = Instant::now() + Duration::from_secs(30);
    while session.exited.is_none() {
        session.pump();
        if Instant::now() > deadline {
            panic!("the shell exit was never reported");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let exited = session.exited.clone().expect("the status arrived");
    assert_eq!(exited, ExitReport::Code { code: 4 });
    assert!(exited.is_abnormal());
}

#[test]
fn a_program_that_exits_immediately_after_output_is_still_read() {
    // A shell that prints and exits at once is the case where a naive reader loses
    // the output or waits for ever.
    let (program, args) = if cfg!(windows) {
        ("cmd.exe", vec!["/c", "echo final-words"])
    } else {
        ("sh", vec!["-c", "echo final-words"])
    };
    let Some(mut session) = Session::start_program(program, &args) else {
        return;
    };

    let screen = session.wait_for("the final output", contains("final-words"));
    assert!(screen.contains("final-words"), "{screen}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while session.exited.is_none() {
        session.pump();
        if Instant::now() > deadline {
            panic!("the exit status never arrived");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(session.exited, Some(ExitReport::Clean));
}

#[test]
fn sustained_output_can_be_interrupted_and_the_shell_stays_usable() {
    let Some(mut session) = Session::start() else {
        return;
    };

    if cfg!(windows) {
        session.run("1..2000 | ForEach-Object { \"tick $_\"; Start-Sleep -Milliseconds 5 }");
    } else {
        session.run("while true; do echo tick; sleep 0.01; done");
    }
    // The program is producing output continuously, so the screen must keep moving.
    session.wait_for("sustained output", |_snapshot, text| {
        text.matches("tick").count() >= 3
    });
    // Then the user interrupts it with the control key a terminal must send.
    session.write(&[0x03]);

    // The shell has to come back and accept another command: the interruption must
    // stop the program, not the session. Nothing here waits a guessed amount of
    // time: a line typed while the interrupted program has not stopped yet is
    // swallowed by that program, so the same line is offered again on the polling
    // loop and the screen decides when the shell took it.
    //
    // The marker is split across two literals in the command itself, because a
    // terminal echoes what is typed: a literal marker would let the echo satisfy
    // the wait before the command had run. Only real output contains it whole.
    let command = if cfg!(windows) {
        "Write-Output ('after' + '-interrupt')\r"
    } else {
        "echo 'after'-'interrupt'\r"
    };
    let mut next_offer = Instant::now();
    let screen = session.poll_until("the shell to answer again", |session, _snapshot, text| {
        if text.contains("after-interrupt") {
            return true;
        }
        if Instant::now() >= next_offer {
            session.write(command.as_bytes());
            next_offer = Instant::now() + Duration::from_millis(200);
        }
        false
    });
    assert!(
        screen.contains("after-interrupt"),
        "the shell did not recover from the interrupt:\n{screen}"
    );
}
