//! Which shells this machine actually has (R2).
//!
//! A shell picker that offers programs which do not exist is worse than no picker:
//! the user finds out only after choosing one. The list below is built from the
//! places each platform installs shells in, and every entry is checked against the
//! file system before it is offered — the probe is injectable so that rule can be
//! tested without owning the machine's shell installation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use shellune_core::LocalShell;

/// The flag that makes a known shell read the same files a login shell reads.
///
/// `-l` is the spelling zsh, bash, fish and sh all accept for "login shell". A
/// shell outside that list is never given it: a program that does not know the flag
/// exits instead of opening a terminal, which is worse than no login files.
pub const LOGIN_FLAG: &str = "-l";

/// The locale used when the inherited one cannot encode non-ASCII text (R15).
///
/// macOS ships `en_US.UTF-8` in every configuration, so choosing it needs no
/// question to the machine. A locale that only might exist would leave the shell in
/// exactly the broken state the requirement describes.
pub const UTF8_FALLBACK_LOCALE: &str = "en_US.UTF-8";

/// The variables that decide which character set a program decodes, strongest
/// first, in the order the C library applies them.
const LOCALE_VARIABLES: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

/// The variables the terminal sets itself; a login profile cannot override them.
///
/// `TERM` has to describe this terminal, not the one the user last used, and the
/// requirement allows these values to differ from the login shell's.
const TERMINAL_OWNED: [&str; 2] = ["TERM", "COLORTERM"];

/// The program a login shell runs to report its own environment (R14).
const ENVIRONMENT_PROBE: &str = "/usr/bin/env -0";

/// How long the login shell has before the terminal opens without its answer.
///
/// A profile can wait for input or run something slow; a terminal that never opens
/// because of that is the failure this bound prevents.
const ENVIRONMENT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the user is told when the login shell could not be asked, and nothing else.
pub const LOGIN_ENVIRONMENT_NOTE: &str = "the login shell did not report its environment; the local shell starts with the \
     environment the application inherited";

/// What the user is told when their locale was replaced because it is not UTF-8.
pub const LOCALE_REPLACED_NOTE: &str =
    "the configured locale is not UTF-8; the local shell starts in a UTF-8 locale instead";

/// What the user is told when no locale was inherited at all.
pub const LOCALE_ADDED_NOTE: &str =
    "no locale was inherited; the local shell starts in a UTF-8 locale";

/// How a shell has to be started so it reads the user's login configuration (R14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginForm {
    /// The program is known to accept this login flag.
    Flag(&'static str),
    /// The program is not known to need one: it starts exactly as configured.
    AsConfigured,
}

/// How `program` must be started so it behaves as a login shell.
///
/// The decision is made on the file name, because the same shell is installed as
/// `SHELL=/opt/homebrew/bin/fish`, `/bin/zsh` and a bare `bash`.
#[must_use]
pub fn login_form(program: &str) -> LoginForm {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    match name {
        "zsh" | "bash" | "fish" | "sh" => LoginForm::Flag(LOGIN_FLAG),
        _ => LoginForm::AsConfigured,
    }
}

/// Whether a locale name selects UTF-8, in every spelling the C library accepts
/// (`UTF-8`, `utf8`, `en_US.UTF-8`, `C.UTF-8`).
fn is_utf8_locale(name: &str) -> bool {
    name.to_ascii_uppercase()
        .replace(['-', '_'], "")
        .contains("UTF8")
}

/// What a shell environment needs so that Chinese is not mangled (R15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalePlan {
    /// The variable to set to `UTF8_FALLBACK_LOCALE`, or `None` when the inherited
    /// locale already selects UTF-8.
    pub variable: Option<&'static str>,
    /// Whether that variable held a value of the user's that is being replaced
    /// because it cannot encode UTF-8. Missing is not the same as replaced.
    pub replaced: bool,
}

/// Decides whether the inherited locale has to be changed to get UTF-8.
///
/// A value the user set and that is valid UTF-8 is always kept, including a
/// non-English one. The strongest variable decides, as it does in the C library: a
/// non-UTF-8 `LC_ALL` is not repaired by a UTF-8 `LANG`, so `LC_ALL` is what
/// gets a UTF-8 value; nothing else in the user's categories changes.
#[must_use]
pub fn plan_locale(lookup: &dyn Fn(&str) -> Option<String>) -> LocalePlan {
    for variable in LOCALE_VARIABLES {
        let Some(value) = lookup(variable).filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        return if is_utf8_locale(&value) {
            LocalePlan {
                variable: None,
                replaced: false,
            }
        } else {
            LocalePlan {
                variable: Some(variable),
                replaced: true,
            }
        };
    }
    LocalePlan {
        variable: Some("LANG"),
        replaced: false,
    }
}

/// The variables a local shell needs beyond what the application inherited.
///
/// Only what is missing is added: an environment the application already has —
/// because it was started from a terminal, or the user set it deliberately — is
/// never replaced. The notes are worded for the user and never contain a value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellEnvironment {
    variables: Vec<(String, String)>,
    notes: Vec<String>,
}

impl ShellEnvironment {
    /// The variables to set for the child process, in the order they were found.
    #[must_use]
    pub fn variables(&self) -> &[(String, String)] {
        &self.variables
    }

    /// What could not be done as configured, in wording that is safe to show.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    fn get(&self, key: &str) -> Option<String> {
        self.variables
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value.clone())
    }

    fn set(&mut self, key: &str, value: &str) {
        if let Some(existing) = self
            .variables
            .iter_mut()
            .find(|(existing, _)| existing == key)
        {
            existing.1 = value.to_owned();
            return;
        }
        self.variables.push((key.to_owned(), value.to_owned()));
    }

    fn note(&mut self, note: &str) {
        if !self.notes.iter().any(|existing| existing == note) {
            self.notes.push(note.to_owned());
        }
    }
}

/// The environment a local shell gets at startup (R14, R15).
///
/// Only macOS needs this: an application started from the Dock or Finder inherits a
/// minimal environment, while a shell started from a terminal session inherits the
/// user's login environment. Everywhere else nothing is changed, so no other
/// platform starts behaving differently.
#[must_use]
pub fn shell_environment(program: &str) -> ShellEnvironment {
    let mut environment = ShellEnvironment::default();
    if !cfg!(target_os = "macos") {
        return environment;
    }
    match login_environment(program) {
        Some(captured) => {
            let inherited = inherited_variables(captured, &|key| std::env::var_os(key).is_some());
            for (key, value) in inherited {
                environment.set(&key, &value);
            }
        }
        None => environment.note(LOGIN_ENVIRONMENT_NOTE),
    }
    let plan = plan_locale(&|variable| {
        std::env::var(variable)
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| environment.get(variable))
    });
    if let Some(variable) = plan.variable {
        environment.set(variable, UTF8_FALLBACK_LOCALE);
        environment.note(if plan.replaced {
            LOCALE_REPLACED_NOTE
        } else {
            LOCALE_ADDED_NOTE
        });
    }
    environment
}

/// The entries of a login environment the application does not already have.
///
/// A value the application already carries — because it was started from a
/// terminal, or the user set it for the application — is never replaced, and the
/// terminal keeps its own `TERM`.
fn inherited_variables(
    captured: Vec<(String, String)>,
    present: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    captured
        .into_iter()
        .filter(|(key, _)| !TERMINAL_OWNED.contains(&key.as_str()) && !present(key))
        .collect()
}

/// Asks the login shell for the environment it would give a program (R14).
///
/// The probe runs the non-interactive login form, which reads the login files
/// (`.zprofile`, `.zlogin`) but not the interactive ones; a PATH that only `.zshrc`
/// sets is still in place, because the shell started in the PTY is interactive and
/// reads that file itself.
///
/// Returns `None` when the shell cannot be asked — an unknown program, a profile
/// that failed, or one that never finished. A missing answer never stops the shell
/// from starting, and no value that came back is logged.
fn login_environment(program: &str) -> Option<Vec<(String, String)>> {
    let LoginForm::Flag(flag) = login_form(program) else {
        // Not a shell this module knows how to ask; the shell still starts.
        return None;
    };
    let output = probe_environment(program, &[flag, "-c", ENVIRONMENT_PROBE])?;
    Some(parse_environment(&output))
}

/// Runs `program` with fixed arguments and returns its standard output.
///
/// The output is read on its own thread: a large environment fills the pipe long
/// before the shell has written all of it, and a full pipe looks exactly like a slow
/// shell from here.
fn probe_environment(program: &str, arguments: &[&str]) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout.read_to_end(&mut buffer);
        }
        buffer
    });
    let deadline = Instant::now() + ENVIRONMENT_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            // A profile that waits for input, or one that is simply slow: the
            // shell is started without its answer instead of never.
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                // The reader is left to finish on its own: joining it would block
                // the terminal on exactly the profile that did not answer.
                return None;
            }
        }
    }
    reader.join().ok()
}

/// Reads the environment a login shell printed with `env -0` (R14).
///
/// Anything that is not a `NAME=value` entry is ignored: a profile may print a
/// banner, and a banner is not the environment. A value that is not UTF-8 is kept
/// with replacement characters rather than dropped, and nothing here is logged.
#[must_use]
pub fn parse_environment(output: &[u8]) -> Vec<(String, String)> {
    let mut variables: Vec<(String, String)> = Vec::new();
    for entry in output.split(|byte| *byte == 0) {
        let entry = String::from_utf8_lossy(entry);
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        if let Some(existing) = variables.iter_mut().find(|(existing, _)| existing == name) {
            // A later assignment is the one the shell would have exported last.
            existing.1 = value.to_owned();
        } else {
            variables.push((name.to_owned(), value.to_owned()));
        }
    }
    variables
}

/// Shells to try first on this platform, most preferred first.
///
/// The names are display names; the program is what the PTY starts.
fn candidates() -> Vec<(LocalShell, String)> {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        let windows_powershell =
            format!("{system_root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        let cmd = format!("{system_root}\\System32\\cmd.exe");
        vec![
            (
                LocalShell::new("Windows PowerShell", windows_powershell.clone()),
                windows_powershell,
            ),
            (
                LocalShell::new("PowerShell 7", "pwsh.exe"),
                "pwsh.exe".to_owned(),
            ),
            (LocalShell::new("Command Prompt", cmd.clone()), cmd),
        ]
    }
    #[cfg(unix)]
    {
        let mut entries = Vec::new();
        if let Ok(shell) = std::env::var("SHELL")
            && !shell.trim().is_empty()
        {
            let name = Path::new(&shell)
                .file_name()
                .map_or_else(|| shell.clone(), |name| name.to_string_lossy().into_owned());
            entries.push((LocalShell::new(capitalize(&name), shell.clone()), shell));
        }
        for (name, program) in [
            ("Zsh", "/bin/zsh"),
            ("Bash", "/bin/bash"),
            ("Fish", "/usr/bin/fish"),
            ("sh", "/bin/sh"),
        ] {
            entries.push((LocalShell::new(name, program), program.to_owned()));
        }
        entries
    }
    #[cfg(not(any(windows, unix)))]
    {
        Vec::new()
    }
}

#[cfg(unix)]
fn capitalize(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => name.to_owned(),
    }
}

/// Shells that exist on this machine, in preference order.
#[must_use]
pub fn discover() -> Vec<LocalShell> {
    discover_with(&program_exists)
}

/// Shells that exist according to a caller-supplied probe.
///
/// The probe receives the program string exactly as it would be spawned, which is
/// what lets the test describe a machine with only PowerShell installed.
#[must_use]
pub fn discover_with(probe: &dyn Fn(&str) -> bool) -> Vec<LocalShell> {
    let mut shells: Vec<LocalShell> = Vec::new();
    for (shell, program) in candidates() {
        if !probe(&program) {
            continue;
        }
        if shells
            .iter()
            .any(|existing| existing.program() == shell.program())
        {
            continue;
        }
        shells.push(shell);
    }
    shells
}

/// The shell a new local terminal starts with.
///
/// When nothing was found the platform's conventional program is returned anyway:
/// starting it then fails with the real system error, which is more useful than an
/// empty list the user cannot act on.
#[must_use]
pub fn platform_default() -> LocalShell {
    if let Some(shell) = discover().into_iter().next() {
        return shell;
    }
    #[cfg(windows)]
    {
        LocalShell::new("Command Prompt", "cmd.exe")
    }
    #[cfg(unix)]
    {
        let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        LocalShell::new(
            capitalize(
                Path::new(&program)
                    .file_name()
                    .map_or("sh", |name| name.to_str().unwrap_or("sh")),
            ),
            program,
        )
    }
    #[cfg(not(any(windows, unix)))]
    {
        LocalShell::new("Shell", "sh")
    }
}

/// Whether a program can be started by that name on this machine.
#[must_use]
pub fn program_exists(program: &str) -> bool {
    let path = Path::new(program);
    let looks_like_path = program.contains('/') || program.contains('\\');
    if looks_like_path {
        return path.is_file();
    }
    search_path(program).is_some()
}

/// Where a bare program name resolves to, using the same rules the system does.
#[must_use]
pub fn search_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        pathext
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| extension.to_lowercase())
            .collect()
    } else {
        Vec::new()
    };

    for directory in std::env::split_paths(&path) {
        let direct = directory.join(program);
        if direct.is_file() {
            return Some(direct);
        }
        if cfg!(windows) && !program.contains('.') {
            for extension in &extensions {
                let candidate = directory.join(format!("{program}{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_program_is_not_offered() {
        let shells = discover_with(&|_| false);
        assert!(shells.is_empty(), "no probe accepts nothing: {shells:?}");
    }

    #[test]
    fn only_probed_programs_are_offered() {
        #[cfg(windows)]
        let wanted = "pwsh.exe";
        #[cfg(unix)]
        let wanted = "/bin/bash";

        let shells = discover_with(&|program| program == wanted);
        assert_eq!(shells.len(), 1, "got {shells:?}");
        assert_eq!(shells[0].program(), wanted);
    }

    #[test]
    fn the_list_is_deduplicated_by_program() {
        let shells = discover_with(&|_| true);
        let mut programs: Vec<&str> = shells.iter().map(LocalShell::program).collect();
        let count = programs.len();
        programs.sort_unstable();
        programs.dedup();
        assert_eq!(programs.len(), count);
    }

    #[test]
    fn every_offered_shell_has_a_name_and_a_program() {
        for shell in discover_with(&|_| true) {
            assert!(!shell.name().trim().is_empty());
            assert!(!shell.program().trim().is_empty());
        }
    }

    #[test]
    fn discovering_this_machine_finds_at_least_one_shell() {
        let shells = discover();
        assert!(
            !shells.is_empty(),
            "a machine running this test must have a shell to offer"
        );
        for shell in &shells {
            assert!(program_exists(shell.program()), "{shell:?} does not exist");
        }
    }

    #[test]
    fn the_default_shell_can_be_started() {
        let shell = platform_default();
        assert!(program_exists(shell.program()), "{shell:?} does not exist");
    }

    #[test]
    fn an_absolute_path_is_probed_directly() {
        let current = std::env::current_exe().expect("the test binary has a path");
        assert!(program_exists(&current.to_string_lossy()));
        assert!(!program_exists("/nonexistent/definitely/missing"));
    }

    #[test]
    fn a_bare_program_name_resolves_through_the_search_path() {
        // Every platform this runs on has a system shell directory on PATH.
        let bare = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let resolved = search_path(bare).unwrap_or_else(|| panic!("PATH lookup failed for {bare}"));
        assert!(resolved.is_file(), "{resolved:?} is not a file");
        assert!(program_exists(bare));
    }

    #[test]
    fn a_program_that_is_not_on_the_path_is_reported_missing() {
        assert!(!program_exists("shellune-no-such-program-9c1"));
        assert_eq!(search_path("shellune-no-such-program-9c1"), None);
    }

    /// A lookup over a fixed set of variables, the way a process reports them.
    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_known_shells_are_started_as_login_shells() {
        for program in [
            "/bin/zsh",
            "zsh",
            "/opt/homebrew/bin/bash",
            "/usr/local/bin/fish",
            "/bin/sh",
        ] {
            assert_eq!(
                login_form(program),
                LoginForm::Flag(LOGIN_FLAG),
                "{program} must read the login files"
            );
        }
    }

    #[test]
    fn an_unknown_shell_starts_exactly_as_configured() {
        // A flag an unknown program does not understand makes it exit, so the only
        // safe degradation is to add nothing.
        for program in [
            "/opt/tools/nu",
            "pwsh.exe",
            "elvish",
            "/bin/zsh-beta",
            "C:\\Windows\\System32\\cmd.exe",
            "",
        ] {
            assert_eq!(
                login_form(program),
                LoginForm::AsConfigured,
                "{program} must not be given a login flag"
            );
        }
    }

    #[test]
    fn a_utf8_locale_is_kept_as_it_is() {
        let cases: &[&[(&str, &str)]] = &[
            &[("LC_ALL", "en_US.UTF-8"), ("LANG", "C")],
            &[("LC_ALL", ""), ("LC_CTYPE", "zh_CN.UTF-8")],
            &[("LANG", "zh_CN.UTF-8")],
            &[("LANG", "C.utf8")],
            &[("LANG", "UTF-8")],
        ];
        for pairs in cases {
            let plan = plan_locale(&lookup(pairs));
            assert_eq!(
                plan,
                LocalePlan {
                    variable: None,
                    replaced: false
                },
                "{pairs:?} is already UTF-8 and must not be touched"
            );
        }
    }

    #[test]
    fn a_missing_locale_falls_back_to_utf8() {
        let plan = plan_locale(&lookup(&[]));
        assert_eq!(
            plan,
            LocalePlan {
                variable: Some("LANG"),
                replaced: false
            },
            "nothing was set, so a UTF-8 default is added and nothing is replaced"
        );
    }

    #[test]
    fn an_unusable_locale_is_replaced_and_reported_as_replaced() {
        let cases: &[(&[(&str, &str)], &str)] = &[
            (&[("LANG", "C")], "LANG"),
            (&[("LANG", "POSIX")], "LANG"),
            (&[("LANG", "en_US")], "LANG"),
            // The strongest variable decides: a UTF-8 LANG does not repair LC_ALL.
            (&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")], "LC_ALL"),
            (&[("LC_CTYPE", "C"), ("LANG", "en_US.UTF-8")], "LC_CTYPE"),
        ];
        for (pairs, variable) in cases {
            let plan = plan_locale(&lookup(pairs));
            assert_eq!(
                plan,
                LocalePlan {
                    variable: Some(variable),
                    replaced: true
                },
                "{pairs:?} cannot encode Chinese and must be replaced visibly"
            );
        }
    }

    #[test]
    fn an_empty_locale_counts_as_missing() {
        let plan = plan_locale(&lookup(&[("LC_ALL", ""), ("LANG", "   ")]));
        assert_eq!(
            plan,
            LocalePlan {
                variable: Some("LANG"),
                replaced: false
            }
        );
    }

    #[test]
    fn the_login_environment_parser_ignores_a_banner_and_keeps_the_last_value() {
        let output = b"Last login: Tue Nov 12\0PATH=/usr/bin\0HOME=/Users/example\0PATH=/usr/bin:/opt/homebrew/bin\0not an entry\0=missing-name\0BAD-NAME=1\0EMPTY=\0";
        let variables = parse_environment(output);
        assert_eq!(
            variables,
            vec![
                ("PATH".to_owned(), "/usr/bin:/opt/homebrew/bin".to_owned()),
                ("HOME".to_owned(), "/Users/example".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ],
            "a banner is not an entry, and the last assignment is the one the shell exported"
        );
    }

    #[test]
    fn a_value_that_is_not_utf8_is_kept_instead_of_dropped() {
        let output = b"LANG=C\0NAME=caf\xe9\0";
        let variables = parse_environment(output);
        assert_eq!(variables.len(), 2, "{variables:?}");
        assert!(
            variables[1].1.contains('\u{fffd}'),
            "an unreadable byte must show as a replacement, not vanish: {variables:?}"
        );
    }

    #[test]
    fn only_missing_variables_are_inherited_and_the_terminal_keeps_its_own() {
        let captured = vec![
            ("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned()),
            ("HOME".to_owned(), "/Users/example".to_owned()),
            ("TERM".to_owned(), "screen-256color".to_owned()),
            ("COLORTERM".to_owned(), "24bit".to_owned()),
        ];
        let inherited = inherited_variables(captured, &|key| key == "HOME");
        assert_eq!(
            inherited,
            vec![("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned())],
            "only the login PATH is added: HOME is already set, and TERM and \
             COLORTERM describe this terminal"
        );
    }

    #[test]
    fn a_login_shell_that_cannot_be_asked_never_blocks_the_terminal() {
        // The program does not exist: the answer is "unknown", not a failure.
        assert_eq!(login_environment("shellune-no-such-login-shell-4d2"), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_keep_the_environment_they_always_had() {
        // The login environment is a macOS problem; changing the variables on
        // Linux or Windows would be an unreviewed behaviour change.
        let environment = shell_environment("/bin/zsh");
        assert!(environment.variables().is_empty(), "{environment:?}");
        assert!(environment.notes().is_empty(), "{environment:?}");
    }
}
