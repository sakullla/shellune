//! Integration tests against a real shell process (R2).
//!
//! These tests start the shell this machine actually has, feed it commands and read
//! what comes back through the same PTY and the same terminal core the application
//! uses. Nothing is stubbed: a passing result here means a real program ran, a real
//! byte stream was parsed and real text landed on the grid.
//!
//! What cannot be checked here is what needs a window: drawing, the input method,
//! clipboard and mouse. Those are covered by the GPUI tests in the application and
//! are listed in the task record, so this file never claims to have verified them.
//!
//! The interrupt cases (R8) are their own binary now, local_interrupt.rs: this machine
//! stops delivering a control byte to a pseudo console once the process has created and
//! closed the terminals of the cases here, which made those four cases skip instead of
//! measure. The rig both files drive is support/session.rs.

use std::time::{Duration, Instant};

use shellune_core::{LocalShell, TerminalMode};
use shellune_terminal::core::SearchOptions;
use shellune_terminal::{
    BlockStatus, ExitReport, GridSize, LocalSession, TermMode, TerminalCore, palette,
    platform_default, program_exists,
};

#[path = "support/session.rs"]
mod session;

use session::{Session, contains};

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

/// Whether this machine can be expected to show Chinese text as text.
///
/// macOS is the platform the requirement covers, and there the application has to
/// supply the UTF-8 locale itself, so the test asserts. Everywhere else the shell can
/// only show what its own locale allows, so a machine without a UTF-8 locale skips
/// instead of reporting a failure the product cannot fix there.
fn chinese_is_expected_to_show() -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .any(|value| {
            value
                .to_uppercase()
                .replace(['-', '_'], "")
                .contains("UTF8")
        })
}

/// The login flag the application adds must not stop a shell from starting.
///
/// This is the risk of the change: a program that does not know `-l` exits at once,
/// and the terminal would look closed. The shell here is one macOS actually has.
#[cfg(unix)]
#[test]
fn a_login_shell_starts_and_runs_a_command() {
    let Some(mut session) = Session::start_shell(LocalShell::new("sh", "/bin/sh")) else {
        eprintln!("no /bin/sh on this machine, skipping");
        return;
    };
    session.run("echo login-shell-ok");
    let screen = session.wait_for("the login shell output", contains("login-shell-ok"));
    assert!(
        screen.contains("login-shell-ok"),
        "a login shell did not run a command:\n{screen}"
    );
}

/// The locale the shell itself resolved, not the one this test process has (R15).
#[cfg(unix)]
#[test]
fn a_local_shell_resolves_an_utf8_locale() {
    if !chinese_is_expected_to_show() {
        eprintln!("this machine has no UTF-8 locale, skipping");
        return;
    }
    let Some(mut session) = Session::start() else {
        return;
    };
    // The strongest variable that is actually set, the way the C library reads it.
    session.run("printf '%s\\n' \"${LC_ALL:-${LC_CTYPE:-$LANG}}\"");
    let screen = session.wait_for("the shell's locale", contains("UTF"));
    assert!(
        screen.contains("UTF"),
        "the local shell did not end up in a UTF-8 locale:\n{screen}"
    );
}

/// What the shell could not be given reaches the window instead of being dropped
/// (R14, R15).
///
/// The session is the only carrier between the environment probe and the bar the user
/// reads, so the notes drawn there have to be exactly the ones that arrived here.
#[test]
fn a_session_hands_the_environment_notes_to_the_window() {
    let Some(session) = Session::start() else {
        return;
    };
    let expected = shellune_terminal::shell::shell_environment(session.shell.program());
    assert_eq!(
        session.pty.environment().notes(),
        expected.notes(),
        "the pane can only show what the session kept"
    );
    #[cfg(not(target_os = "macos"))]
    assert!(
        session.pty.environment().notes().is_empty(),
        "Linux and Windows ask no login shell, so they report no degradation"
    );
}

/// A Chinese file name has to come back as the same text, not as octal escapes or
/// question marks (R15).
///
/// The command contains only the directory, so the file name can reach the screen
/// only through the listing itself and not through the echo of the typed command.
#[test]
fn a_chinese_file_name_survives_a_directory_listing() {
    if !chinese_is_expected_to_show() {
        eprintln!("this machine has no UTF-8 locale, skipping");
        return;
    }
    let Some(mut session) = Session::start() else {
        return;
    };
    let directory = std::env::temp_dir().join("shellune-中文目录-5c1");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a temporary directory can be created");
    std::fs::write(directory.join("说明文件.txt"), b"x").expect("a temporary file can be written");
    let path = directory.display().to_string();
    let command = if cfg!(windows) {
        format!("Get-ChildItem -LiteralPath '{path}' | ForEach-Object Name")
    } else {
        format!("ls '{path}'")
    };
    session.run(&command);
    let screen = session.wait_for("the Chinese file name", contains("说明文件.txt"));
    std::fs::remove_dir_all(&directory).ok();
    assert!(
        screen.contains("说明文件.txt"),
        "the Chinese file name did not survive the listing:\n{screen}"
    );
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
    // The wait is for the block to have *ended*, not for it to exist: the command line
    // reaches the terminal before the marker that closes the block, so a wait that only
    // looks for the name can be answered while the block is still Running - and then the
    // status below is read before the marker that decides it ever arrived. Measured: one
    // serial run in ten failed exactly there, on Running against the expected Unknown.
    session.poll_until("the ended command block", |session, _, _| {
        session.core.command_blocks().iter().any(|block| {
            block.command.contains("false-cmd") && block.status != BlockStatus::Running
        })
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

#[test]
fn history_completion_in_a_local_bash_line_editor() {
    let bash = if cfg!(windows) {
        git_install_path("bash")
    } else {
        Some("bash".to_owned())
    };
    let Some(mut session) =
        bash.and_then(|bash| Session::start_program(&bash, &["--noprofile", "--norc", "-i"]))
    else {
        eprintln!("SKIPPED local completion: Bash PTY unavailable");
        return;
    };
    // Real Bash/Readline owns the edit buffer and its command-name Tab binding.
    // These per-process prompt markers never modify a startup file. macOS ships
    // Bash 3.2 which ignores PS0 (added in 4.4), so fall back to a DEBUG trap there.
    session.run(
        r#"PS1='\[\e]133;A\a\]local> \[\e]133;B\a\]'; PROMPT_COMMAND="printf '\033]133;D;0\a'"; if [ "${BASH_VERSINFO[0]:-0}" -gt 4 ] || { [ "${BASH_VERSINFO[0]:-0}" -eq 4 ] && [ "${BASH_VERSINFO[1]:-0}" -ge 4 ]; }; then PS0=$'\e]133;C\a'; else trap 'case "$BASH_COMMAND" in printf*) ;; *) printf "\033]133;C\a" ;; esac' DEBUG; fi"#,
    );
    session.wait_for("local prompt ready", |_, screen| {
        screen.lines().any(|row| row.trim_end() == "local>")
    });
    session.run("echo local-completion");
    session.poll_until("Bash recorded the executed command", |session, _, _| {
        session
            .core
            .command_blocks()
            .iter()
            .any(|block| block.command == "echo local-completion" && block.status.is_success())
    });
    session.write(b"echo loc");
    session.poll_until(
        "the echoed local prefix has a history candidate",
        |session, _, _| session.core.history_completion() == Some("al-completion"),
    );
    let suffix = session.core.history_completion().unwrap().to_owned();
    session.write(suffix.as_bytes());
    session.write(b"-edited\r");
    session.poll_until("the edit and its output", |session, _, _| {
        session.core.command_blocks().iter().any(|block| {
            block.command == "echo local-completion-edited"
                && block.output.contains("local-completion-edited")
        })
    });
    session.write(b"pushdxyz");
    session.wait_for("unmatched prefix echoed", contains("local> pushdxyz"));
    assert_eq!(session.core.history_completion(), None);
    session.write(b"\x15pushd\t/");
    session.wait_for(
        "Bash Tab completes the builtin name",
        contains("local> pushd /"),
    );
    session.close();
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
