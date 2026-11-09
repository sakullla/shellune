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

use shellune_core::{LocalShell, TerminalMode};
use shellune_terminal::core::SearchOptions;
use shellune_terminal::{
    BlockStatus, ExitReport, GridSize, LocalSession, PtyEvent, Snapshot, TermMode, TerminalCore,
    encode_text, palette, platform_default, program_exists,
};

#[cfg(feature = "test-support")]
mod support;

/// A shell, its PTY and the terminal that parses what it writes.
struct Session {
    shell: LocalShell,
    pty: LocalSession,
    core: TerminalCore,
    exited: Option<ExitReport>,
    /// Bytes the shell has produced since it started.
    ///
    /// This is the only monotonic progress signal a test can rely on: the visible
    /// grid has 30 rows and saturates, so "the screen stopped changing" is also
    /// what a screen that is merely full looks like.
    bytes_read: u64,
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
            bytes_read: 0,
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
            bytes_read: 0,
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
    ///
    /// Returns how many bytes arrived, and adds them to the session total.
    fn pump(&mut self) -> usize {
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
    fn bytes_read(&self) -> u64 {
        self.bytes_read
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
    // A directory whose name the test chooses, so the check does not depend on how
    // the shell spells the temporary folder: on the Windows CI runner the path Rust
    // reports and the one PowerShell prints differ (short 8.3 name against long name),
    // and canonicalising yields an extended-length path (\\?\C:...) no shell prints.
    let name = format!("shellune-cd-{}", std::process::id());
    let target = std::env::temp_dir().join(&name);
    std::fs::create_dir_all(&target).expect("the test directory can be created");
    let path = target.to_string_lossy().into_owned();

    if cfg!(windows) {
        session.run(&format!("Set-Location '{path}'; (Get-Location).Path"));
    } else {
        session.run(&format!("cd '{path}' && pwd"));
    }
    // The command line names the directory once; the shell's answer is a second
    // occurrence, so two mean the change really happened and was reported.
    let screen = session.wait_for("the new directory", |_snapshot, text| {
        text.matches(name.as_str()).count() >= 2
    });
    assert!(
        screen.matches(name.as_str()).count() >= 2,
        "the new directory was not reported: expected {name} in\n{screen}"
    );
    // The shell may still be inside it (Windows refuses to remove it then); a left-over
    // empty folder in the temporary directory is harmless.
    let _ = std::fs::remove_dir(&target);
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
            bytes_read: 0,
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
            bytes_read: 0,
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
fn a_real_shell_reports_command_blocks_without_using_its_process_exit() {
    let Some(mut session) = integration_session() else {
        eprintln!("no shell that can print integration markers, skipping");
        return;
    };
    session.core.set_presentation(TerminalMode::Integrated);

    let start = shell_print(
        &session,
        &format!(
            "{}{}echo hello{}{}hello-block{}",
            osc("133;A"),
            osc("133;B"),
            osc("133;C"),
            "\r\n",
            "\r\n"
        ),
    );
    session.run(&start);
    session.poll_until("the running command block", |session, _, _| {
        session.core.command_blocks().iter().any(|block| {
            block.command.contains("echo hello")
                && block.output.contains("hello-block")
                && !block.status.is_success()
        })
    });
    std::thread::sleep(Duration::from_millis(30));
    session.run(&shell_print(&session, &osc("133;D;0")));
    session.poll_until("the finished command block", |session, _, _| {
        session.core.command_blocks().iter().any(|block| {
            block.command.contains("echo hello")
                && block.status.is_success()
                && block.duration.is_some()
        })
    });
    let finished = session
        .core
        .command_blocks()
        .into_iter()
        .find(|block| block.command.contains("echo hello"))
        .expect("the finished block");
    assert_eq!(finished.status, BlockStatus::Exited(0));
    assert!(finished.output.contains("hello-block"));
    let duration = finished.duration.expect("the markers measured a duration");
    assert!(
        duration.as_millis() >= 1,
        "duration comes from the marker timestamps, got {duration:?}"
    );

    session.run(&shell_print(
        &session,
        &format!(
            "{}{}false-cmd{}{}nope{}{}",
            osc("133;A"),
            osc("133;B"),
            osc("133;C"),
            "\r\n",
            "\r\n",
            osc("133;D")
        ),
    ));
    session.poll_until("the unknown command block", |session, _, _| {
        session
            .core
            .command_blocks()
            .iter()
            .any(|block| block.command.contains("false-cmd"))
    });
    let unknown = session
        .core
        .command_blocks()
        .into_iter()
        .find(|block| block.command.contains("false-cmd"))
        .expect("the block with no exit code");
    assert_eq!(unknown.status, BlockStatus::Unknown);
    assert!(
        !unknown.status.is_success(),
        "a marker with no exit code is not success"
    );
    assert!(unknown.output.contains("nope"));

    let before_alt = session
        .core
        .search_history("hello-block", SearchOptions::literal())
        .expect("a literal query compiles");
    assert!(!before_alt.is_empty(), "the output is in the real grid");
    session.run(&shell_print(&session, "\u{1b}[?1049h"));
    session.wait_for("the alternate screen", |snapshot, _| {
        snapshot.mode.contains(TermMode::ALT_SCREEN)
    });
    assert!(
        session.core.shows_classic_grid(),
        "a full-screen program uses the classic grid"
    );
    session.run(&shell_print(&session, "\u{1b}[?1049l"));
    session.wait_for("the primary screen", |snapshot, _| {
        !snapshot.mode.contains(TermMode::ALT_SCREEN)
    });
    assert!(
        !session
            .core
            .search_history("hello-block", SearchOptions::literal())
            .expect("a literal query compiles")
            .is_empty(),
        "leaving the alternate screen keeps the earlier output"
    );
    assert!(session.core.shows_command_blocks());

    session.run("exit 9");
    let deadline = Instant::now() + Duration::from_secs(30);
    while session.exited.is_none() {
        session.pump();
        if Instant::now() > deadline {
            panic!("the shell exit was never reported");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(session.exited, Some(ExitReport::Code { code: 9 }));
    let after = session.core.command_blocks();
    let hello = after
        .iter()
        .find(|block| block.command.contains("echo hello"))
        .expect("the first block survives the process exit");
    let failed = after
        .iter()
        .find(|block| block.command.contains("false-cmd"))
        .expect("the unknown block survives the process exit");
    assert_eq!(hello.status, BlockStatus::Exited(0));
    assert!(hello.status.is_success());
    assert_eq!(failed.status, BlockStatus::Unknown);
    assert!(
        !failed.status.is_success(),
        "the process exit code is not the command's exit code"
    );
    assert!(
        after
            .iter()
            .all(|block| block.status != BlockStatus::Exited(9)),
        "no command block was rewritten from the process status: {after:?}"
    );
}

fn integration_session() -> Option<Session> {
    if cfg!(windows)
        && let Some(bash) = git_install_path("bash")
    {
        let shell = LocalShell::new("Git Bash", bash);
        let grid = GridSize::new(100, 30);
        let pty = LocalSession::spawn(&shell, grid, None).ok()?;
        return Some(Session {
            shell,
            pty,
            core: TerminalCore::new(grid.cols, grid.rows, 2000),
            exited: None,
            bytes_read: 0,
        });
    }
    Session::start()
}

fn osc(payload: &str) -> String {
    format!("\u{1b}]{payload}\u{7}")
}

fn shell_print(session: &Session, text: &str) -> String {
    let program = session.shell.program().to_ascii_lowercase();
    if program.contains("powershell") || program.contains("pwsh") {
        powershell_print(text)
    } else if program.ends_with("cmd.exe") || program.ends_with("\\cmd") || program == "cmd" {
        format!(
            "powershell -NoProfile -Command \"{}\"",
            powershell_print(text)
        )
    } else {
        posix_printf(text)
    }
}

fn powershell_print(text: &str) -> String {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let flush = |literal: &mut String, parts: &mut Vec<String>| {
        if literal.is_empty() {
            return;
        }
        let escaped = literal.replace('\'', "''");
        parts.push(format!("'{escaped}'"));
        literal.clear();
    };
    for character in text.chars() {
        match character {
            '\u{1b}' => {
                flush(&mut literal, &mut parts);
                parts.push("[char]27".to_owned());
            }
            '\u{7}' => {
                flush(&mut literal, &mut parts);
                parts.push("[char]7".to_owned());
            }
            '\r' => {
                flush(&mut literal, &mut parts);
                parts.push("[char]13".to_owned());
            }
            '\n' => {
                flush(&mut literal, &mut parts);
                parts.push("[char]10".to_owned());
            }
            _ => literal.push(character),
        }
    }
    flush(&mut literal, &mut parts);
    if parts.is_empty() {
        parts.push("''".to_owned());
    }
    format!("Write-Host -NoNewline ({})", parts.join("+"))
}

fn posix_printf(text: &str) -> String {
    let mut body = String::from("printf '");
    for byte in text.bytes() {
        match byte {
            b'\'' => body.push_str("'\\''"),
            0x20..=0x7e => body.push(byte as char),
            _ => body.push_str(&format!("\\{byte:03o}")),
        }
    }
    body.push('\'');
    body
}

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
        loop {
            session.pump();
            let text = session.core.snapshot().text().join("\n");
            if text.matches(INPUT_MARKER).count() >= 2 {
                return;
            }
            if Instant::now() >= next_offer {
                session.run(&format!("echo {INPUT_MARKER}"));
                next_offer = Instant::now() + Duration::from_millis(500);
            }
            assert!(
                Instant::now() < deadline,
                "the shell never answered a typed line in {:?}, so no command could reach it",
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
                "local_shell",
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
                "local_shell",
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
                    "local_shell",
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
        let Some(mut session) = Session::start() else {
            support::report(
                "local_shell",
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
        let Some(session) = shell_for_the_flood() else {
            support::report(
                "local_shell",
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
        let Some(session) = Session::start() else {
            support::report(
                "local_shell",
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
    fn forwards_the_interrupt(mut session: Session, label: &str) {
        let _alone = support::exclusive();
        wait_until_it_takes_input(&mut session);
        let before_launch = session.bytes_read();
        let command = flood_command_for(&session.shell);
        session.run(&command);

        // The command has to be running before the byte is sent, otherwise the press
        // interrupts the prompt instead of the program.
        let started = Instant::now();
        loop {
            session.pump();
            let text = session.core.snapshot().text().join("\n");
            if session.bytes_read() >= before_launch + support::OUTPUT_BEFORE_INTERRUPT
                && text.contains("tick ")
            {
                break;
            }
            assert!(
                started.elapsed() < support::FLOOD_BUDGET,
                "the flood never started from a shell prompt: {} bytes came back after the \
                 command; the screen held:\n{text}",
                session.bytes_read() - before_launch
            );
            std::thread::sleep(support::READ_INTERVAL);
        }

        match support::interrupt_flood(&mut session) {
            Outcome::Stopped {
                bytes_at_press,
                stopped_after,
                presses,
            } => support::report(
                "local_shell",
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
                    "local_shell",
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
                "local_shell",
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
                "local_shell",
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
                    "local_shell",
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
