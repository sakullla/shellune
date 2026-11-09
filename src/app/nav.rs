//! Destination model for the navigation rail (R12).
//!
//! Three pages are navigated: the hosts home (local shells and saved servers are
//! one list, they only connect differently), the open terminals, and the settings.
//! Transfers and forwards remain destinations of the model but open as tools of a
//! host or a session. The list is a model, not a layout: a future layout can render it
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

    /// Stable identifier of the platform, written next to saved key bindings.
    ///
    /// Bindings are kept per platform because Ctrl and Cmd are different keys: a
    /// combination that is free on Windows may belong to the system on macOS.
    pub fn id(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Linux => "linux",
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

    /// Keystroke that opens the command palette.
    ///
    /// Shifted on every platform: Ctrl+P is the shell's "previous command".
    pub fn palette_keystroke(self) -> &'static str {
        match self {
            Self::MacOs => "cmd-shift-p",
            Self::Windows | Self::Linux => "ctrl-shift-p",
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

    /// Keystrokes that select the next and the previous tab.
    pub fn tab_cycle_keystrokes(self) -> (&'static str, &'static str) {
        match self {
            Self::MacOs => ("cmd-shift-]", "cmd-shift-["),
            Self::Windows | Self::Linux => ("ctrl-pagedown", "ctrl-pageup"),
        }
    }

    /// Keystroke that searches the pane that has the keyboard (R1).
    ///
    /// ⌘F is the find shortcut a macOS user already knows and no shell program reads
    /// it. Ctrl+F moves the cursor forward in a shell, so Windows and Linux take the
    /// shifted form, as their other terminals do.
    pub fn search_keystroke(self) -> &'static str {
        match self {
            Self::MacOs => "cmd-f",
            Self::Windows | Self::Linux => "ctrl-shift-f",
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
    /// Local, remote and dynamic forwarding rules.
    Forwards,
    /// Appearance, language, terminal and data settings.
    Settings,
}

impl Destination {
    /// Every destination. Each has its own focus handle.
    pub const ALL: [Self; 6] = [
        Self::LocalShells,
        Self::Hosts,
        Self::Sessions,
        Self::Transfers,
        Self::Forwards,
        Self::Settings,
    ];

    /// The pages a user moves between: the hosts home (local shells and saved
    /// servers in one list), the open terminals, and the settings. Transfers and
    /// forwards are tools of a host or a session, opened from there; the old session
    /// list repeated the terminal tabs and is gone.
    pub const PAGES: [Self; 3] = [Self::Hosts, Self::LocalShells, Self::Settings];

    /// Position in [`Destination::ALL`].
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|destination| *destination == self)
            .unwrap_or(0)
    }

    /// Whether this destination is one of [`Destination::PAGES`].
    #[cfg(test)]
    pub fn is_page(self) -> bool {
        Self::PAGES.contains(&self)
    }

    /// Destination name in the current language.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::LocalShells => strings.nav_local,
            Self::Hosts => strings.nav_hosts,
            Self::Sessions => strings.nav_sessions,
            Self::Transfers => strings.nav_transfers,
            Self::Forwards => strings.nav_forwards,
            Self::Settings => crate::app::copy::ux().settings_title,
        }
    }

    /// One-line description of what the destination holds.
    pub fn summary(self, strings: &Strings) -> &'static str {
        match self {
            Self::LocalShells => strings.nav_local_summary,
            Self::Hosts => strings.nav_hosts_summary,
            Self::Sessions => strings.nav_sessions_summary,
            Self::Transfers => strings.nav_transfers_summary,
            Self::Forwards => strings.nav_forwards_summary,
            Self::Settings => crate::app::copy::ux().settings_summary,
        }
    }

    /// Debug selector of the navigation entry, used by the layout tests.
    #[cfg(test)]
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::LocalShells => "nav.local",
            Self::Hosts => "nav.hosts",
            Self::Sessions => "nav.sessions",
            Self::Transfers => "nav.transfers",
            Self::Forwards => "nav.forwards",
            Self::Settings => "nav.settings",
        }
    }

    /// Digit shown in the shortcut, one-based: the place among
    /// [`Destination::PAGES`]. A tool destination has none and reports 0.
    pub fn shortcut_digit(self) -> usize {
        Self::PAGES
            .iter()
            .position(|page| *page == self)
            .map_or(0, |index| index + 1)
    }

    /// Keystroke string accepted by [`gpui::KeyBinding::new`].
    ///
    /// The readable label of the same shortcut is the key map's job: it knows the
    /// binding in force, which may no longer be this default (R5).
    pub fn keystroke(self, platform: Platform) -> String {
        format!(
            "{}-{}",
            platform.keystroke_modifier(),
            self.shortcut_digit()
        )
    }

    fn page_index(self) -> usize {
        Self::PAGES
            .iter()
            .position(|page| *page == self)
            .unwrap_or(0)
    }

    /// The next page, wrapping around, for the "next section" shortcut.
    pub fn next(self) -> Self {
        Self::PAGES[(self.page_index() + 1) % Self::PAGES.len()]
    }

    /// The previous page, wrapping around.
    pub fn previous(self) -> Self {
        Self::PAGES[(self.page_index() + Self::PAGES.len() - 1) % Self::PAGES.len()]
    }
}
