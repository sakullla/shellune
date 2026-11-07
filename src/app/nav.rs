//! Destination model for the navigation rail (R12).
//!
//! Navigation is a fixed list of four destinations — local shells, hosts,
//! sessions and transfers — because those are the objects the product organises
//! work around. The list is a model, not a layout: a future layout can render it
//! as a rail, a tab strip or a palette without touching the destinations.
//!
//! Shortcut labels are platform-specific on purpose: a macOS build shows and
//! binds ⌘1 while Windows and Linux show Ctrl+1, which is the convention users of
//! each system expect.

use crate::app::copy::Strings;

/// The platforms whose keyboard convention the shell follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux.
    Linux,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// Symbol or word used in front of a digit in a shortcut label.
    pub fn shortcut_modifier(self) -> &'static str {
        match self {
            Self::MacOs => "⌘",
            Self::Windows | Self::Linux => "Ctrl",
        }
    }

    /// Prefix accepted by the GPUI keymap for this platform.
    pub fn keystroke_modifier(self) -> &'static str {
        match self {
            Self::MacOs => "cmd",
            Self::Windows | Self::Linux => "ctrl",
        }
    }

    /// Keystroke that opens a new local shell, in the GPUI keymap syntax.
    ///
    /// Ctrl+T alone belongs to programs running in the terminal on Windows and
    /// Linux, so the shifted form is used there, as other terminals do.
    pub fn new_shell_keystroke(self) -> &'static str {
        match self {
            Self::MacOs => "cmd-t",
            Self::Windows | Self::Linux => "ctrl-shift-t",
        }
    }

    /// Label of [`Platform::new_shell_keystroke`].
    pub fn new_shell_label(self) -> &'static str {
        match self {
            Self::MacOs => "\u{2318}T",
            Self::Windows | Self::Linux => "Ctrl+Shift+T",
        }
    }

    /// Keystroke that opens the command palette.
    ///
    /// Shifted on every platform: Ctrl+P is the shell's "previous command".
    pub fn palette_keystroke(self) -> &'static str {
        match self {
            Self::MacOs => "cmd-shift-p",
            Self::Windows | Self::Linux => "ctrl-shift-p",
        }
    }

    /// Label of [`Platform::palette_keystroke`].
    pub fn palette_label(self) -> &'static str {
        match self {
            Self::MacOs => "\u{21e7}\u{2318}P",
            Self::Windows | Self::Linux => "Ctrl+Shift+P",
        }
    }

    /// Keystroke that collapses or expands the sidebar.
    ///
    /// Shifted outside macOS: Ctrl+B is the tmux prefix, typed all day in a terminal.
    pub fn sidebar_keystroke(self) -> &'static str {
        match self {
            Self::MacOs => "cmd-b",
            Self::Windows | Self::Linux => "ctrl-shift-b",
        }
    }

    /// Label of [`Platform::sidebar_keystroke`].
    pub fn sidebar_label(self) -> &'static str {
        match self {
            Self::MacOs => "\u{2318}B",
            Self::Windows | Self::Linux => "Ctrl+Shift+B",
        }
    }

    /// Keystroke that closes the session on screen.
    ///
    /// Ctrl+W alone deletes a word in the shell, so the shifted form is used outside
    /// macOS, as other terminals do.
    pub fn close_tab_keystroke(self) -> &'static str {
        match self {
            Self::MacOs => "cmd-w",
            Self::Windows | Self::Linux => "ctrl-shift-w",
        }
    }

    /// Label of [`Platform::close_tab_keystroke`].
    pub fn close_tab_label(self) -> &'static str {
        match self {
            Self::MacOs => "\u{2318}W",
            Self::Windows | Self::Linux => "Ctrl+Shift+W",
        }
    }

    /// Keystrokes that select the next and the previous tab.
    pub fn tab_cycle_keystrokes(self) -> (&'static str, &'static str) {
        match self {
            Self::MacOs => ("cmd-shift-]", "cmd-shift-["),
            Self::Windows | Self::Linux => ("ctrl-pagedown", "ctrl-pageup"),
        }
    }

    /// Modifier of the shortcuts that jump straight to tab 1-9.
    ///
    /// The command key already selects a page on macOS, and Ctrl+digit does on
    /// Windows and Linux, so tabs take the next free modifier.
    pub fn tab_jump_modifier(self) -> &'static str {
        match self {
            Self::MacOs => "ctrl",
            Self::Windows | Self::Linux => "alt",
        }
    }

    /// Label of the tab jump shortcut for tab `number`.
    pub fn tab_jump_label(self, number: usize) -> String {
        match self {
            Self::MacOs => format!("\u{2303}{number}"),
            Self::Windows | Self::Linux => format!("Alt+{number}"),
        }
    }

    /// The whole navigation range, for example "Ctrl+1-4" or "⌘1-4".
    ///
    /// Derived from the same modifier and digits as `Destination::shortcut_label`
    /// so the hint written in the rail cannot name a key this platform does not
    /// bind.
    pub fn shortcut_range(self) -> String {
        let first = Destination::ALL[0].shortcut_digit();
        let last = Destination::ALL[Destination::ALL.len() - 1].shortcut_digit();
        match self {
            Self::MacOs => format!("{}{}-{}", self.shortcut_modifier(), first, last),
            Self::Windows | Self::Linux => {
                format!("{}+{}-{}", self.shortcut_modifier(), first, last)
            }
        }
    }
}

/// One top-level place in the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Destination {
    /// Shells running on this machine.
    LocalShells,
    /// Saved SSH hosts and their settings.
    Hosts,
    /// Every open terminal, local or remote.
    Sessions,
    /// Upload and download tasks.
    Transfers,
}

impl Destination {
    /// Every destination, in rail order.
    pub const ALL: [Self; 4] = [
        Self::LocalShells,
        Self::Hosts,
        Self::Sessions,
        Self::Transfers,
    ];

    /// Position in [`Destination::ALL`].
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|destination| *destination == self)
            .unwrap_or(0)
    }

    /// Destination name in the current language.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::LocalShells => strings.nav_local,
            Self::Hosts => strings.nav_hosts,
            Self::Sessions => strings.nav_sessions,
            Self::Transfers => strings.nav_transfers,
        }
    }

    /// One-line description of what the destination holds.
    pub fn summary(self, strings: &Strings) -> &'static str {
        match self {
            Self::LocalShells => strings.nav_local_summary,
            Self::Hosts => strings.nav_hosts_summary,
            Self::Sessions => strings.nav_sessions_summary,
            Self::Transfers => strings.nav_transfers_summary,
        }
    }

    /// Unit word used after a count of the objects this destination holds.
    ///
    /// One unit per destination, so a count of hosts is never printed with the
    /// word "session".
    pub fn count_unit(self, strings: &Strings) -> &'static str {
        match self {
            Self::LocalShells | Self::Sessions => strings.count_sessions,
            Self::Hosts => strings.count_hosts,
            Self::Transfers => strings.count_transfers,
        }
    }

    /// Debug selector of the navigation entry, used by the layout tests.
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::LocalShells => "nav.local",
            Self::Hosts => "nav.hosts",
            Self::Sessions => "nav.sessions",
            Self::Transfers => "nav.transfers",
        }
    }

    /// Digit shown in the shortcut, one-based.
    pub fn shortcut_digit(self) -> usize {
        self.index() + 1
    }

    /// Shortcut label for the given platform, for example "Ctrl+1" or "⌘1".
    pub fn shortcut_label(self, platform: Platform) -> String {
        match platform {
            Platform::MacOs => format!("{}{}", platform.shortcut_modifier(), self.shortcut_digit()),
            Platform::Windows | Platform::Linux => {
                format!("{}+{}", platform.shortcut_modifier(), self.shortcut_digit())
            }
        }
    }

    /// Keystroke string accepted by [`gpui::KeyBinding::new`].
    pub fn keystroke(self, platform: Platform) -> String {
        format!(
            "{}-{}",
            platform.keystroke_modifier(),
            self.shortcut_digit()
        )
    }

    /// The next destination, wrapping around, for the "next section" shortcut.
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The previous destination, wrapping around.
    pub fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}
