//! Shell state: destinations, sections and honest section status (R12, R18).
//!
//! The shell is written so that a section can only look successful when it holds
//! data that justifies it. [`SectionStatus`] separates three situations that a
//! naive "empty list" would collapse into one:
//!
//! * `Loading` — work is in flight and nothing is known yet;
//! * `Failed` — the last attempt failed, with a reason and a recovery step;
//! * `Ready` — the system reported a real result, which may still be empty.
//!
//! Only `Ready` with at least one item can render as populated, so the shell
//! cannot show a live session, a saved host or a finished transfer that no layer
//! ever produced.

use shellune_core::{
    ForwardRule, HostConfig, HostId, LocalShell, PaneId, PaneTarget, PanelConfig, PanelSection,
    Session, SessionFailure, SessionId, SessionKind, SessionState, SplitAxis, Transfer,
    WorkspaceSnapshot,
};

use crate::app::layout::SessionLayout;

use crate::app::copy::{Locale, Strings};
use crate::app::nav::Destination;
use crate::app::theme::{Metrics, Motion, Tone, Zoom};

/// What the user can do about a failure.
///
/// The variants are constructed by the layers that can fail - a host file that cannot
/// be read produces [`Recovery::Retry`] today - and the rest document the recovery
/// paths the shell must be able to show before a layer needs them.
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
/// Every capability this build ships produces one of these three: a section that has
/// not been read yet is `Loading`, not "planned", because there is no capability left
/// that this build does not have.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionStatus {
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
            Self::Loading => strings.state_loading,
            Self::Empty => strings.state_empty,
            Self::Failed => strings.state_failed,
            Self::Populated => strings.state_ready,
        }
    }

    /// Whether the state may be drawn as a success.
    ///
    /// This is the single place that decides it, so a view cannot mark a loading or
    /// failed section as done.
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
    ///
    /// Every main button of the redesigned header does real work (the transfers page
    /// leads to a file view or to the hosts), so the window no longer raises this; the
    /// model keeps the honest answer for a request that has no implementation.
    #[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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
    motion: Motion,
    sessions: Section<Session>,
    hosts: Section<HostConfig>,
    transfers: Section<Transfer>,
    forwards: Section<ForwardRule>,
    shells: Vec<LocalShell>,
    shell_choice: usize,
    local_font_size: f32,
    notice: Option<Notice>,
    /// Which context panel is beside the session workspace.
    context: PanelSection,
    /// Tabs and the pane tree. Leaves name sessions; they do not own a second copy.
    layout: SessionLayout,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    /// A workspace where every capability is implemented and no data has been read yet.
    ///
    /// Sessions and transfers start as real, empty results: an empty page means
    /// "nothing is open" rather than "not built". Hosts start as `Loading`, because the
    /// saved records really are unread until the shell has read the store, and a page
    /// that showed "no hosts" before the file was read would be a claim no layer made.
    pub fn new() -> Self {
        Self {
            destination: Destination::LocalShells,
            locale: Locale::default(),
            zoom: Zoom::default(),
            motion: Motion::default(),
            // The local terminal is implemented, so its section starts as a real,
            // empty result instead of unread work. Which shell can be started is
            // decided by probing this machine, not by the build.
            sessions: Section::ready(Vec::new()),
            // Reading the store is what fills this in, so it starts unread.
            hosts: Section::loading(),
            // The transfer list is a real, empty result: a task appears in it only
            // because a worker reported it, which is what lets an empty page mean
            // "nothing was transferred" (R10).
            transfers: Section::ready(Vec::new()),
            forwards: Section::ready(Vec::new()),
            shells: shellune_terminal::discover(),
            shell_choice: 0,
            local_font_size: shellune_terminal::DEFAULT_FONT_SIZE,
            notice: None,
            context: PanelSection::None,
            layout: SessionLayout::new(),
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

    /// The context panel selected beside the session workspace.
    pub fn context(&self) -> PanelSection {
        self.context
    }

    /// Records which context panel is open. This does not start a task.
    pub fn set_context(&mut self, section: PanelSection) {
        self.context = section;
    }

    /// Tabs and panes. Reading this does not start a connection.
    #[must_use]
    pub fn layout(&self) -> &SessionLayout {
        &self.layout
    }

    /// The mutable layout, for a placement the view has already decided.
    pub fn layout_mut(&mut self) -> &mut SessionLayout {
        &mut self.layout
    }

    /// The session each open tab is known by, in tab order.
    #[must_use]
    pub fn tab_anchors(&self) -> Vec<SessionId> {
        self.layout.tab_anchors()
    }

    /// The next session splits the active pane instead of opening a tab.
    pub fn arm_split(&mut self, axis: SplitAxis) -> bool {
        self.layout.arm_split(axis)
    }

    /// The next session fills a pane that is already waiting.
    pub fn arm_adopt(&mut self, pane: PaneId) {
        self.layout.arm_adopt(pane);
    }

    /// Forgets a split or adopt that the start did not use.
    pub fn clear_armed(&mut self) {
        self.layout.clear_armed();
    }

    /// The layout as a snapshot. No password, socket or process is included.
    ///
    /// # Errors
    ///
    /// Returns the reason the tree could not be written.
    pub fn capture_snapshot(&self) -> Result<WorkspaceSnapshot, String> {
        let panel = PanelConfig::new(self.context != PanelSection::None, self.context);
        self.layout
            .capture(
                |id| {
                    self.session(id)
                        .map(|session| session.title().to_owned())
                        .unwrap_or_else(|| "Session".to_owned())
                },
                panel,
            )
            .map_err(|error| error.to_string())
    }

    /// Replaces the layout with pending panes from a restore.
    ///
    /// The sessions that are already open are not started or stopped here. A
    /// restored leaf has no session until the user connects it.
    pub fn install_restored(&mut self, restored: shellune_core::RestoredWorkspace) {
        self.context = restored.panel().section();
        self.layout.install_restored(restored);
    }

    /// Marks only the remote panes whose host is no longer saved.
    pub fn recheck_layout_hosts(&mut self, exists: impl FnMut(HostId) -> bool) {
        self.layout.recheck_hosts(exists);
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

    /// Cycles the text size through 90%, 100%, 115% and 200%.
    ///
    /// One control is easier to explain than a pair of buttons in a shell that only
    /// needs a larger or smaller label. 200% is the ceiling the layout keeps usable.
    pub fn cycle_text_size(&mut self) {
        self.zoom = self.zoom.next();
    }

    /// Whether short interface motion is played. This is not read from the OS.
    pub fn motion(&self) -> Motion {
        self.motion
    }

    /// Switches between full motion and reduced motion.
    pub fn cycle_motion(&mut self) {
        self.motion = self.motion.next();
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

    /// Replaces the transfer list with what the transfer view really holds.
    ///
    /// The items come from the same queue the panel reads, so the page cannot show a
    /// task the panel does not have, and a completed transfer is listed as completed on
    /// both.
    pub fn replace_transfers(&mut self, items: Vec<Transfer>) {
        self.transfers.replace(SectionStatus::Ready, items);
    }

    /// Saved forwarding rules. Replacing the list does not start a listener.
    pub fn replace_forwards(&mut self, items: Vec<ForwardRule>) {
        self.forwards.replace(SectionStatus::Ready, items);
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
            // A failed launch is local. Other local sessions stay listed, and a remote
            // session alone does not make the local page look populated.
            Destination::LocalShells => {
                let any_local = self.local_sessions().next().is_some();
                match self.sessions.status() {
                    SectionStatus::Loading if !any_local => SectionState::Loading,
                    SectionStatus::Failed(_) if !any_local => SectionState::Failed,
                    _ if !any_local => SectionState::Empty,
                    _ => SectionState::Populated,
                }
            }
            Destination::Hosts => self.hosts.state(),
            // One bad launch must not replace the session list with a single failure card.
            Destination::Sessions => match self.sessions.status() {
                SectionStatus::Failed(_) if !self.sessions.items().is_empty() => {
                    SectionState::Populated
                }
                _ => self.sessions.state(),
            },
            Destination::Transfers => self.transfers.state(),
            Destination::Forwards => self.forwards.state(),
        }
    }

    /// Number of items a destination would list.
    pub fn item_count(&self, destination: Destination) -> usize {
        match destination {
            Destination::LocalShells => self.local_sessions().count(),
            Destination::Hosts => self.hosts.count(),
            Destination::Sessions => self.sessions.count(),
            Destination::Transfers => self.transfers.count(),
            Destination::Forwards => self.forwards.count(),
        }
    }

    /// The failure behind a destination, when it has one.
    pub fn failure(&self, destination: Destination) -> Option<&Failure> {
        let status = match destination {
            Destination::LocalShells | Destination::Sessions => self.sessions.status(),
            Destination::Hosts => self.hosts.status(),
            Destination::Transfers => self.transfers.status(),
            Destination::Forwards => self.forwards.status(),
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
    #[cfg_attr(not(test), allow(dead_code))]
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
    #[cfg_attr(not(test), allow(dead_code))]
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
        self.layout.attach(
            session.id(),
            PaneTarget::local(Some(shell.program().to_owned())),
            session.title(),
        );
        session
    }

    /// Creates a session record for a host that is being connected to.
    ///
    /// The record starts in `Connecting`, so a host that never answers is listed
    /// as a session that did not open rather than one that is live.
    pub fn register_remote_session(&mut self, host: &HostConfig) -> Session {
        let mut session = Session::new(SessionKind::Remote { host: host.id() }, host.name());
        let _ = session.begin_connecting();
        let mut sessions = self.sessions.items().to_vec();
        sessions.push(session.clone());
        self.sessions.replace(SectionStatus::Ready, sessions);
        self.layout
            .attach(session.id(), PaneTarget::remote(host.id()), session.title());
        session
    }

    /// Records that a remote session verified the host identity and authenticated.
    ///
    /// The two steps are one call because they happen in one place: the user
    /// answered the identity question and the server accepted the credentials.
    pub fn session_verified_and_authenticated(&mut self, id: SessionId) -> bool {
        self.transition(id, |session| {
            session.begin_host_verification().is_ok() && session.begin_authentication().is_ok()
        })
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

    /// Replaces the reason an ended session states.
    ///
    /// Used when a reconnect is refused: the session stays ended, but the reason now says
    /// that another attempt cannot work, so the surface can name the step that does (R12).
    pub fn session_end_restated(&mut self, id: SessionId, failure: SessionFailure) -> bool {
        self.transition(id, |session| session.restate_end(failure).is_ok())
    }

    /// Records that the user closed a session.
    pub fn session_closed(&mut self, id: SessionId) -> bool {
        let closed = self.transition(id, |session| session.close().is_ok());
        if closed {
            self.layout.detach_session(id);
        }
        closed
    }

    /// Records a failure that stopped a shell from starting at all.
    ///
    /// No session is created for a process that never ran, so there is no phantom
    /// terminal. The sessions that are already open are kept: one wrong shell path
    /// says nothing about the terminals that are running, and dropping them would
    /// close what the user did not ask to close.
    pub fn fail_local_start(&mut self, failure: Failure) {
        let sessions = self.sessions.items().to_vec();
        self.sessions
            .replace(SectionStatus::Failed(failure), sessions);
    }

    /// Drops a launch failure once a later start really produced a session.
    ///
    /// The failure belonged to the launch, not to the sessions that are open. Leaving
    /// the section failed would keep a banner over terminals that started afterwards.
    pub fn clear_session_failure(&mut self) {
        if matches!(self.sessions.status(), SectionStatus::Failed(_)) {
            let items = self.sessions.items().to_vec();
            self.sessions.replace(SectionStatus::Ready, items);
        }
    }

    /// Replaces the session list, for the model tests that pin one rule at a time.
    #[cfg(test)]
    pub fn set_sessions_for_test(&mut self, sessions: Vec<Session>) {
        self.sessions.replace(SectionStatus::Ready, sessions);
    }

    /// One session, by identity.
    #[must_use]
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions
            .items()
            .iter()
            .find(|session| session.id() == id)
    }

    /// Whether an event from one process of a session still belongs to it.
    ///
    /// True only for the process the session currently runs: a late event from a
    /// process that ended, or from one that a reconnect replaced, is not.
    #[must_use]
    pub fn session_accepts_event(&self, tag: shellune_core::EventTag) -> bool {
        self.session(tag.session)
            .is_some_and(|session| session.accepts_event(tag.generation))
    }

    /// Drops a record for a session whose process never ran.
    ///
    /// A shell that refused to start is not a session: the user did not get a
    /// terminal, so leaving a record behind would list something that never existed.
    /// Only an attempt that never became live may be forgotten this way.
    pub fn session_forgotten(&mut self, id: SessionId) {
        self.layout.detach_session(id);
        self.forget_session_record(id);
    }

    /// Undoes an adopt whose process never started.
    ///
    /// The leaf goes back to waiting, with the target it had. A launch that opened
    /// a new tab is not this case: [`Self::session_forgotten`] still removes it.
    pub fn revert_failed_adopt(&mut self, id: SessionId) -> bool {
        if !self.layout.revert_adopted(id) {
            return false;
        }
        self.forget_session_record(id);
        true
    }

    /// A started shell owns the leaf, so a later failure must not restore the old one.
    pub fn clear_adopted_launch(&mut self) {
        self.layout.clear_adopted();
    }

    fn forget_session_record(&mut self, id: SessionId) {
        let remaining: Vec<Session> = self
            .sessions
            .items()
            .iter()
            .filter(|session| session.id() != id)
            .cloned()
            .collect();
        let status = match self.sessions.status() {
            SectionStatus::Failed(_) => self.sessions.status().clone(),
            _ => SectionStatus::Ready,
        };
        self.sessions.replace(status, remaining);
    }

    /// The sessions whose tab was closed: records the user can remove.
    pub fn closed_sessions(&self) -> Vec<SessionId> {
        self.sessions
            .items()
            .iter()
            .filter(|session| matches!(session.state(), SessionState::Closed))
            .map(Session::id)
            .collect()
    }

    /// Removes the record of a session whose tab was closed.
    ///
    /// Only a closed session may go: a record that still has a tab is what the tab
    /// reports from, so removing it would leave a terminal nobody can describe.
    /// Returns whether the record was removed.
    pub fn session_removed(&mut self, id: SessionId) -> bool {
        let closed = self
            .session(id)
            .is_some_and(|session| matches!(session.state(), SessionState::Closed));
        if closed {
            self.session_forgotten(id);
        }
        closed
    }

    /// Records that the user asked a session that ended to start another process.
    ///
    /// The record is the same one, so the tab keeps its name and its position; the
    /// generation moves on, which is what ends the previous process' claim on it.
    ///
    /// Returns whether the session could be restarted: a live, pending or closed one
    /// cannot.
    pub fn session_reconnecting(&mut self, id: SessionId) -> bool {
        self.transition(id, |session| session.begin_reconnect().is_ok())
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
