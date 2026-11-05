//! A shell program the user can run on this machine (R2).
//!
//! The type carries what a picker and a session title need — a human name and the
//! program to start — and nothing about how it is spawned. Spawning belongs to the
//! terminal crate, which is the layer that can report a real launch failure, so
//! this model stays usable in a window-less test.

use serde::{Deserialize, Serialize};

/// One shell executable available on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalShell {
    name: String,
    program: String,
}

impl LocalShell {
    /// Builds a shell entry from a display name and the program to start.
    ///
    /// An empty name falls back to the program, so a picker never shows a blank
    /// entry.
    #[must_use]
    pub fn new(name: impl Into<String>, program: impl Into<String>) -> Self {
        let program = program.into();
        let name = name.into();
        let name = if name.trim().is_empty() {
            program.clone()
        } else {
            name.trim().to_owned()
        };
        Self { name, program }
    }

    /// Name shown in the shell picker and in the session title.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Program passed to the PTY.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }
}

impl std::fmt::Display for LocalShell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_name_falls_back_to_the_program() {
        let shell = LocalShell::new("  ", "pwsh.exe");
        assert_eq!(shell.name(), "pwsh.exe");
        assert_eq!(shell.program(), "pwsh.exe");
        assert_eq!(shell.to_string(), "pwsh.exe");
    }

    #[test]
    fn a_named_shell_keeps_both_fields() {
        let shell = LocalShell::new("PowerShell 7", "pwsh.exe");
        assert_eq!(shell.name(), "PowerShell 7");
        assert_eq!(shell.program(), "pwsh.exe");
        assert_ne!(shell.name(), shell.program());
    }
}
