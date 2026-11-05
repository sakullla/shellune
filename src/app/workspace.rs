//! Shell state: destinations, sections and honest section status (R12, R18).
//!
//! The shell is written so that a section can only look successful when it holds
//! data that justifies it. [`SectionStatus`] separates four situations that a
//! naive "empty list" would collapse into one:
//!
//! * `Planned` — this build does not ship the capability, and no work exists;
//! * `Loading` — work is in flight and nothing is known yet;
//! * `Failed` — the last attempt failed, with a reason and a recovery step;
//! * `Ready` — the system reported a real result, which may still be empty.
//!
//! Only `Ready` with at least one item can render as populated, so the shell
//! cannot show a live session, a saved host or a finished transfer that no layer
//! ever produced.

use shellune_core::{
    HostConfig, LocalShell, Session, SessionFailure, SessionId, SessionKind, Transfer,
};

use crate::app::copy::{Locale, Strings};
use crate::app::nav::Destination;
use crate::app::theme::{Metrics, Tone, Zoom};

/// What the user can do about a failure.
///
/// The variants are constructed by the layers that can fail; until they exist
/// (`SectionStatus::Failed` is only reachable from tests in this build) the
/// variant list documents the recovery paths the shell must be able to show.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Repeat the failed operation.
    Retry,
    /// Create a host configuration so a connection has a target.
    CreateHost,
    /// Import an existing SSH configuration.
    ImportConfig,
    /// Pick a different local shell.
    ChooseShell,
}

impl Recovery {
    /// Localized action label.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::Retry => strings.recovery_retry,
            Self::CreateHost => strings.recovery_create_host,
            Self::ImportConfig => strings.recovery_import_config,
            Self::ChooseShell => strings.recovery_choose_shell,
        }
    }
}

/// A failure the shell can show without guessing at a cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    reason: String,
    recovery: Recovery,
}

impl Failure {
    /// Builds a failure from a reason produced by a lower layer.
    #[allow(dead_code)]
    pub fn new(reason: impl Into<String>, recovery: Recovery) -> Self {
        Self {
            reason: reason.into(),
            recovery,
        }
    }

    /// What went wrong, in the words of the layer that knows.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// What the user can do next.
    pub fn recovery(&self) -> Recovery {
        self.recovery
    }
}

/// Reliability of the data behind a section.
///
/// `Loading`, `Failed` and `Ready` are constructed by the steps that own a
/// section; this build only produces `Planned` at runtime and exercises the other
/// three through tests.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionStatus {
    /// Not implemented by this build; deliberately not an error.
    Planned,
    /// A request is in flight.
    Loading,
    /// The last request failed.
    Failed(Failure),
    /// The data is a real result from the layer that owns it.
    Ready,
}

/// How a section should be drawn, derived from its status and contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionState {
    /// The capability is not part of this build.
    Planned,
    /// A request is in flight.
    Loading,
    /// A real result, and it is empty.
    Empty,
    /// A real failure.
    Failed,
    /// Real items exist.
    Populated,
}

impl SectionState {
    /// Tone that carries the meaning of the state.
    pub fn tone(self) -> Tone {
        match self {
            Self::Planned => Tone::Planned,
            Self::Loading => Tone::Warning,
            Self::Empty => Tone::Neutral,
            Self::Failed => Tone::Danger,
            Self::Populated => Tone::Accent,
        }
    }

    /// Chip label for the state.
    ///
    /// Every state has one, including a populated section: the chip reports the
    /// status and the count beside it reports the amount. Reusing a unit word as
    /// the chip would say "session" where the reader expects a state.
    pub fn chip_label(self, strings: &Strings) -> &'static str {
        match self {
            Self::Planned => strings.state_planned,
            Self::Loading => strings.state_loading,
            Self::Empty => strings.state_empty,
            Self::Failed => strings.state_failed,
            Self::Populated => strings.state_ready,
        }
    }

    /// Whether the state may be drawn as a success.
    ///
    /// This is the single place that decides it, so a view cannot mark a planned
    /// or failed section as done.
    #[allow(dead_code)]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Populated)
    }
}

/// One section of the shell and the data behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section<T> {
    status: SectionStatus,
    items: Vec<T>,
}

impl<T> Section<T> {
    /// A section this build does not implement yet.
    pub fn planned() -> Self {
        Self {
            status: SectionStatus::Planned,
            items: Vec::new(),
        }
    }

    /// A section whose request is in flight.
    #[allow(dead_code)]
    pub fn loading() -> Self {
        Self {
            status: SectionStatus::Loading,
            items: Vec::new(),
        }
    }

    /// A section holding a real, possibly empty, result.
    #[allow(dead_code)]
    pub fn ready(items: Vec<T>) -> Self {
        Self {
            status: SectionStatus::Ready,
            items,
        }
    }

    /// A section whose last request failed.
    #[allow(dead_code)]
    pub fn failed(failure: Failure) -> Self {
        Self {
            status: SectionStatus::Failed(failure),
            items: Vec::new(),
        }
    }

    /// Current status.
    pub fn status(&self) -> &SectionStatus {
        &self.status
    }

    /// Items reported by the owning layer.
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Items, so a session can record a lifecycle change in place.
    pub fn items_mut(&mut self) -> &mut [T] {
        &mut self.items
    }

    /// Replaces the whole result, keeping the status that justifies it.
    pub fn replace(&mut self, status: SectionStatus, items: Vec<T>) {
        self.status = status;
        self.items = items;
    }

    /// Number of items.
    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// How the section should be drawn.
    pub fn state(&self) -> SectionState {
        match self.status {
            SectionStatus::Planned => SectionState::Planned,
            SectionStatus::Loading => SectionState::Loading,
            SectionStatus::Failed(_) => SectionState::Failed,
            SectionStatus::Ready if self.items.is_empty() => SectionState::Empty,
            SectionStatus::Ready => SectionState::Populated,
        }
    }
}

/// Why a notice is shown under the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The requested action does not exist in this build.
    UnavailableBuild,
}

impl Notice {
    /// Localized notice text.
    pub fn text(self, strings: &Strings) -> &'static str {
        match self {
            Self::UnavailableBuild => strings.unavailable_notice,
        }
    }
}

/// What happened when the header's primary action was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimaryOutcome {
    /// Nothing was started, and the user was told why.
    Unavailable,
    /// The section that owns the action handled it.
    Delegated,
}

/// The whole shell state.
#[derive(Debug)]
pub struct Workspace {
    destination: Destination,
    locale: Locale,
    zoom: Zoom,
    sessions: Section<Session>,
    hosts: Section<HostConfig>,
    transfers: Section<Transfer>,
    shells: Vec<LocalShell>,
    shell_choice: usize,
    local_font_size: f32,
    notice: Option<Notice>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    /// A workspace where every capability is still unimplemented.
    ///
    /// Later tasks replace one section at a time with [`Section::ready`], which is
    /// why the constructor starts from `Planned` rather than from empty lists.
    pub fn new() -> Self {
        Self {
            destination: Destination::LocalShells,
            locale: Locale::default(),
            zoom: Zoom::default(),
            // The local terminal is implemented, so its section starts as a real,
            // empty result instead of planned work. Which shell can be started is
            // decided by probing this machine, not by the build.
            sessions: Section::ready(Vec::new()),
            hosts: Section::planned(),
            transfers: Section::planned(),
            shells: shellune_terminal::discover(),
            shell_choice: 0,
            local_font_size: shellune_terminal::DEFAULT_FONT_SIZE,
            notice: None,
        }
    }

    /// A workspace with explicit sections, used by tests and by later tasks.
    #[allow(dead_code)]
    pub fn with_sections(
        sessions: Section<Session>,
        hosts: Section<HostConfig>,
        transfers: Section<Transfer>,
    ) -> Self {
        Self {
            sessions,
            hosts,
            transfers,
            ..Self::new()
        }
    }

    /// The destination currently shown.
    pub fn destination(&self) -> Destination {
        self.destination
    }

    /// Shows another destination and clears any stale notice.
    pub fn select(&mut self, destination: Destination) {
        if self.destination != destination {
            self.destination = destination;
            self.notice = None;
        }
    }

    /// Advances to the next destination.
    pub fn select_next(&mut self) {
        self.select(self.destination.next());
    }

    /// Goes back to the previous destination.
    pub fn select_previous(&mut self) {
        self.select(self.destination.previous());
    }

    /// UI language.
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// The active string table.
    pub fn strings(&self) -> &'static Strings {
        self.locale.strings()
    }

    /// Switches the language.
    pub fn toggle_locale(&mut self) {
        self.locale = self.locale.toggle();
    }

    /// The text-size preference.
    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// Metrics derived from the current zoom.
    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.zoom)
    }

    /// Cycles the text size through the three levels.
    ///
    /// One control with three steps is easier to explain than a pair of buttons in
    /// a shell that only needs a larger or smaller label.
    pub fn cycle_text_size(&mut self) {
        self.zoom = self.zoom.next();
    }

    /// The notice currently shown, if any.
    pub fn notice(&self) -> Option<Notice> {
        self.notice
    }

    /// Removes the notice.
    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// All sessions, in the order they were opened.
    pub fn sessions(&self) -> &Section<Session> {
        &self.sessions
    }

    /// Saved hosts.
    pub fn hosts(&self) -> &Section<HostConfig> {
        &self.hosts
    }

    /// Replaces the saved hosts with what the storage layer really holds.
    ///
    /// The section is only ever filled from a loaded or saved file, so a populated
    /// hosts list cannot appear before a store produced it.
    pub fn replace_hosts(&mut self, status: SectionStatus, items: Vec<HostConfig>) {
        self.hosts.replace(status, items);
    }

    /// Reports that the host file could not be read.
    ///
    /// The list is emptied in the same step: showing the previous records beside a
    /// failure would claim they are still what is on disk.
    pub fn fail_hosts(&mut self, failure: Failure) {
        self.hosts
            .replace(SectionStatus::Failed(failure), Vec::new());
    }

    /// Transfer tasks.
    pub fn transfers(&self) -> &Section<Transfer> {
        &self.transfers
    }

    /// Sessions that run on this machine.
    pub fn local_sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions
            .items()
            .iter()
            .filter(|session| matches!(session.kind(), SessionKind::Local { .. }))
    }

    /// How a destination should be drawn.
    ///
    /// The local shells destination shares the session section but filters it, so
    /// "no local session" and "no session at all" stay distinguishable.
    pub fn state(&self, destination: Destination) -> SectionState {
        match destination {
            Destination::LocalShells => match self.sessions.status() {
                SectionStatus::Ready if self.local_sessions().next().is_none() => {
                    SectionState::Empty
                }
                _ => self.sessions.state(),
            },
            Destination::Hosts => self.hosts.state(),
            Destination::Sessions => self.sessions.state(),
            Destination::Transfers => self.transfers.state(),
        }
    }

    /// Number of items a destination would list.
    pub fn item_count(&self, destination: Destination) -> usize {
        match destination {
            Destination::LocalShells => self.local_sessions().count(),
            Destination::Hosts => self.hosts.count(),
            Destination::Sessions => self.sessions.count(),
            Destination::Transfers => self.transfers.count(),
        }
    }

    /// The failure behind a destination, when it has one.
    pub fn failure(&self, destination: Destination) -> Option<&Failure> {
        let status = match destination {
            Destination::LocalShells | Destination::Sessions => self.sessions.status(),
            Destination::Hosts => self.hosts.status(),
            Destination::Transfers => self.transfers.status(),
        };
        match status {
            SectionStatus::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    /// Runs the primary action of the current destination.
    ///
    /// A capability this build does not implement reports
    /// [`PrimaryOutcome::Unavailable`] and shows a notice instead of pretending the
    /// action happened.
    pub fn request_primary(&mut self) -> PrimaryOutcome {
        if !self.can_act(self.destination) {
            self.notice = Some(Notice::UnavailableBuild);
            return PrimaryOutcome::Unavailable;
        }
        PrimaryOutcome::Delegated
    }

    /// Whether the primary action of a destination has an implementation.
    ///
    /// The local terminal can always be attempted: a machine with no shell still
    /// gets a real answer from the launch, which is more honest than greying the
    /// action out. Saving a host is the same kind of action: the store answers
    /// whether it could be written, so the button is never greyed out on a guess.
    /// The other destinations arrive with their own steps.
    pub fn can_act(&self, destination: Destination) -> bool {
        matches!(destination, Destination::LocalShells | Destination::Hosts)
    }

    /// Shells that exist on this machine.
    pub fn shells(&self) -> &[LocalShell] {
        &self.shells
    }

    /// Replaces the discovered shells, keeping the selection inside the list.
    #[cfg(test)]
    pub fn set_shells(&mut self, shells: Vec<LocalShell>) {
        self.shells = shells;
        if self.shell_choice >= self.shells.len() {
            self.shell_choice = 0;
        }
    }

    /// Index of the shell the primary action starts.
    pub fn shell_choice(&self) -> usize {
        self.shell_choice
    }

    /// The shell the primary action starts.
    pub fn selected_shell(&self) -> Option<&LocalShell> {
        self.shells.get(self.shell_choice)
    }

    /// Chooses which shell the primary action starts.
    pub fn select_shell(&mut self, index: usize) {
        if index < self.shells.len() {
            self.shell_choice = index;
        }
    }

    /// Font size the terminal starts at and remembers across sessions.
    pub fn local_font_size(&self) -> f32 {
        self.local_font_size
    }

    /// Records the font size the user chose in the terminal.
    pub fn set_local_font_size(&mut self, size: f32) {
        self.local_font_size = shellune_terminal::sanitize_font_size(size);
    }

    /// Creates a session record for a local shell that is starting.
    pub fn register_local_session(&mut self, shell: &LocalShell) -> Session {
        let mut session = Session::new(
            SessionKind::Local {
                shell: Some(shell.program().to_owned()),
            },
            shell.name(),
        );
        // A session that has not been connected yet must not be listed as live.
        let _ = session.begin_connecting();
        let mut sessions = self.sessions.items().to_vec();
        sessions.push(session.clone());
        self.sessions.replace(SectionStatus::Ready, sessions);
        session
    }

    /// Records that a local session reached the live state.
    pub fn session_connected(&mut self, id: SessionId) -> bool {
        self.transition(id, |session| session.connected().is_ok())
    }

    /// Records why a session stopped being live.
    pub fn session_failed(&mut self, id: SessionId, failure: SessionFailure) -> bool {
        self.transition(id, |session| {
            if session.state().was_live() {
                session.disconnected(failure).is_ok()
            } else {
                session.fail(failure).is_ok()
            }
        })
    }

    /// Records that the user closed a session.
    pub fn session_closed(&mut self, id: SessionId) -> bool {
        self.transition(id, |session| session.close().is_ok())
    }

    /// Records a failure that stopped a shell from starting at all.
    ///
    /// The session list is left empty and the section becomes a failure, so no
    /// phantom terminal appears where the shell refused to run.
    pub fn fail_local_start(&mut self, failure: Failure) {
        self.sessions
            .replace(SectionStatus::Failed(failure), Vec::new());
    }

    fn transition(&mut self, id: SessionId, change: impl FnOnce(&mut Session) -> bool) -> bool {
        let Some(session) = self
            .sessions
            .items_mut()
            .iter_mut()
            .find(|session| session.id() == id)
        else {
            return false;
        };
        change(session)
    }
}
