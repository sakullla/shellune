//! Which shells this machine actually has (R2).
//!
//! A shell picker that offers programs which do not exist is worse than no picker:
//! the user finds out only after choosing one. The list below is built from the
//! places each platform installs shells in, and every entry is checked against the
//! file system before it is offered — the probe is injectable so that rule can be
//! tested without owning the machine's shell installation.

use std::path::{Path, PathBuf};

use shellune_core::LocalShell;

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
}
