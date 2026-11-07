//! The Shellune shell view: navigation rail, header, destination panel, status bar.
//!
//! The composition follows the navigation model in [`crate::app::nav`] and the
//! state model in [`crate::app::workspace`]. Render functions never decide whether a
//! capability exists: they read [`SectionState`] and draw what it reports, which is
//! what keeps a loading or failed section from looking finished.
//!
//! Layout, in one sentence: a sidebar on the left (destinations, then the language
//! and text-size settings at its foot), a header that names the current page with
//! its one main action - or, while a terminal is on screen, holds the session tabs
//! instead - and the destination panel: an edge-to-edge terminal, a welcome page, or
//! a centred reading column that scrolls.

use gpui::prelude::*;
use gpui::{
    Context, Div, Entity, FocusHandle, FontWeight, IntoElement, Render, SharedString, Stateful,
    Window, div, px,
};
use shellune_core::{
    BrowseState, EventTag, FileKind, HostConfig, HostId, LocalShell, Session, SessionFailure,
    SessionId, SessionKind, SessionState, Transfer, join_path,
};
use shellune_ssh::{ConnectRequest, PtySize, RemoteEvent, RemoteSession};
use shellune_storage::{
    HostBook, HostStore, HostTrust, ImportOutcome, ImportPreview, ImportedHost, SecretStore,
    TrustStore,
};
use shellune_terminal::GridSize;

use crate::app::actions::{
    CloseActiveTab, CycleTextSize, DismissNotice, GoToHosts, GoToLocalShells, GoToSessions,
    GoToTransfers, NewLocalShell, NextDestination, NextTab, OpenPalette, PreviousDestination,
    PreviousTab, SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5, SelectTab6,
    SelectTab7, SelectTab8, SelectTab9, ToggleLanguage, ToggleSidebar,
};
use crate::app::components::Variant;
use crate::app::copy::{self, Locale, Strings};
#[cfg(test)]
use crate::app::files::BrowseChannel;
use crate::app::files::{
    FileViews, FilesDialog, kind_label, path_problem_text, problem_text, transfer_problem_text,
};
use crate::app::hosts::{
    HostDraft, HostField, HostView, HostsPanel, ImportPanel, format_unix, group_row_offsets,
    import_message,
};
use crate::app::icons::{self, Icon};
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::remote::{self, MethodChoice, RemoteFailure, RemotePanel, RemoteStatus};
use crate::app::terminal::{self, PaneState, SourceEvent, TerminalPane};
use crate::app::text_field::{Browse, TextField};
use crate::app::theme::{self, Metrics, Tone, palette, spacing};
#[cfg(test)]
use crate::app::transfers::TransferChannel;
use crate::app::transfers::{
    PathProblem, TransferProblem, TransferViews, default_download_directory, file_name,
};
use crate::app::workspace::{Failure, Recovery, SectionState, SectionStatus, Workspace};
use crate::app::{components, theme::radius};

mod host_dialog;
mod overlays;
mod sessions;
mod titlebar;
pub use titlebar::{TRAFFIC_LIGHT_POSITION, custom_titlebar};

/// Where a terminal comes from.
///
/// A session keeps its kind for its whole life. Local and remote sessions share the
/// workspace; the kind is written on the session bar and the tab, not by sending the
/// user to a different page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneKind {
    Local,
    Remote,
}

impl PaneKind {
    /// The kind of terminal a session runs.
    fn of(kind: &SessionKind) -> Self {
        if kind.is_remote() {
            Self::Remote
        } else {
            Self::Local
        }
    }
}

/// One terminal and the session it shows.
///
/// The pair is the unit of everything multi-session: an event, a close or a reconnect
/// names a session, and the entry that carries that session is the only one touched.
struct OpenPane {
    /// Which session process the terminal shows.
    tag: EventTag,
    /// The terminal itself.
    pane: Entity<TerminalPane>,
}

/// One connection attempt.
///
/// Several may be in flight together. Each owns its session, its transport and, when
/// it is not the form on screen, its own panel. Cancelling one does not touch the
/// others. `session` is `None` after the transport has ended but the form is still
/// showing the reason.
struct Attempt {
    /// The session record the attempt owns.
    id: SessionId,
    /// The session generation this transport belongs to.
    generation: u32,
    /// The transport, until it becomes a terminal or ends.
    session: Option<RemoteSession>,
    /// Whether a poll loop is already watching it.
    watching: bool,
    /// The form while another attempt is the one on screen.
    ///
    /// `None` means the form is [`ShelluneView::remote`] because this attempt is focused.
    panel: Option<RemotePanel>,
    /// The host was not written to the host file.
    ephemeral: bool,
}

/// How often the shell looks for events from a connecting remote session.
///
/// Short enough that a trust question appears as the handshake reaches it, long
/// enough that a slow connection does not cost a frame per byte.
pub const REMOTE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Height of the remote terminal inside the host destination.
///
/// A terminal squeezed into a few rows cannot show a command and its answer, so the
/// panel keeps a size that is worth typing into.
pub const REMOTE_TERMINAL_HEIGHT: f32 = 340.0;

/// Width of the file view beside the terminal.
///
/// Beside rather than above: a directory list and a terminal are two things a user
/// reads at the same time, and stacking them leaves neither enough rows.
pub const FILES_PANEL_WIDTH: f32 = 340.0;

/// How often the shell looks for answers from a file view.
///
/// Short enough that a directory appears as soon as the server answers, long enough
/// that an idle file view costs almost nothing.
pub const FILES_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);

/// How many items of a change report the panel lists.
///
/// A recursive delete of a large tree can reach hundreds of paths; the count line
/// carries the whole number, so the list can stay a report and not a log.
pub const FILES_REPORT_STEPS: usize = 12;

/// Ends a menu entry that asks for more input before it acts.
const ELLIPSIS: &str = "\u{2026}";

/// The application shell view.
pub struct ShelluneView {
    workspace: Workspace,
    root_focus: FocusHandle,
    destination_focus: Vec<FocusHandle>,
    /// Every open terminal, in the order its session was opened.
    ///
    /// Sessions are kept apart by identity, never by position: nothing here reaches
    /// for "the current terminal" to decide where an event belongs.
    panes: Vec<OpenPane>,
    /// The session whose terminal is on screen.
    active: Option<SessionId>,
    /// Why the last local start failed, when it did.
    ///
    /// Kept beside the sessions instead of replacing them: a shell path that did not
    /// start says nothing about the terminals that are already running.
    local_error: Option<String>,
    shell_path: Entity<TextField>,
    custom_shell_open: bool,
    /// Whether the "new shell" menu in the header is open.
    ///
    /// The menu is how another shell is started while a terminal fills the panel; the
    /// empty destination shows the same choices as tiles instead.
    shell_menu_open: bool,
    store: HostStore,
    /// What that file really held the last time it was read or written.
    book: HostBook,
    /// The host list views, the open form and the last message.
    hosts: HostsPanel,
    hosts_search: Entity<TextField>,
    /// The six form fields, present only while the editor is open.
    host_fields: Option<HostFields>,
    /// The SSH configuration import panel and what the last read of a file showed.
    import: ImportPanel,
    /// The configuration path field, present only while that panel is open.
    import_path: Option<Entity<TextField>>,
    /// The connection form on screen. Other attempts keep theirs on [`Attempt::panel`].
    remote: Option<RemotePanel>,
    /// Every connection attempt that has not become a terminal, focused or not.
    attempts: Vec<Attempt>,
    /// The session the open form belongs to, once an attempt exists for it.
    form_session: Option<SessionId>,
    /// The quick-connection form is open.
    quick_open: bool,
    /// Why the quick-connection form refused to start.
    quick_error: Option<String>,
    /// The quick connection signs in with the system agent instead of a password.
    quick_agent: bool,
    /// Address, port, user and password of a connection that is not saved first.
    quick_address: Entity<TextField>,
    quick_port: Entity<TextField>,
    quick_user: Entity<TextField>,
    quick_password: Entity<TextField>,
    /// Host used by the next [`Self::connect_remote`] when it must not be saved yet.
    pending_ephemeral: Option<HostConfig>,
    /// Successful quick connections the user has not saved as hosts.
    unsaved: Vec<(SessionId, HostConfig)>,
    /// The connection form is what the workspace is showing, instead of a terminal.
    showing_connection_form: bool,
    /// The ended session a new attempt restarts, when the form was opened by a
    /// reconnect instead of by a host row.
    reconnect: Option<SessionId>,
    /// The credentials fields, created once and reused per attempt.
    remote_fields: RemoteFields,
    /// The accepted server identities.
    trust: TrustStore,
    /// What that file really held the last time it was read or written.
    trust_book: HostTrust,
    /// Why the trust file could not be read, when it could not.
    trust_error: Option<String>,
    /// The file views of the remote sessions, one per browsable connection.
    files: FileViews,
    /// The transfers of the remote sessions and how far each one is (R10, R17).
    transfers: TransferViews,
    /// Why the last upload or download could not start at all (R10).
    ///
    /// A path this window refuses keeps its own question open; a connection that cannot
    /// take the command has no field to point at, so the reason is kept here and shown
    /// with the transfer list of that session.
    transfer_problem: Option<(SessionId, TransferProblem)>,
    /// The session whose close is waiting for the user's answer (R10).
    ///
    /// It exists only while a session with active transfers has been asked about:
    /// closing it cancels those transfers, and that is a consequence the user has to see
    /// before it happens.
    closing: Option<SessionId>,
    /// Whether the file views are being polled for answers.
    files_polling: bool,
    /// The change the file view is asking about before anything is sent (R11).
    files_dialog: Option<FilesDialog>,
    /// The field of that question, present only while it has one.
    files_field: Option<Entity<TextField>>,
    /// Where remembered passwords and passphrases are kept: the system credential store,
    /// or memory in tests.
    secrets: std::sync::Arc<dyn SecretStore>,
    /// The command palette's search field, present only while the palette is open.
    palette: Option<Entity<TextField>>,
    /// What the palette's field held when the list was last ranked.
    palette_query: String,
    /// The highlighted palette entry.
    palette_selected: usize,
    /// Keeps the highlighted palette entry in view.
    palette_scroll: gpui::ScrollHandle,
    /// The tab whose menu is open, and where the pointer opened it.
    tab_menu: Option<(SessionId, gpui::Point<gpui::Pixels>)>,
    /// Whether the sessions page shows the selected terminal rather than the list.
    ///
    /// Selecting a tab shows its terminal; going to the page from the sidebar or its
    /// shortcut shows every session instead, which is the only place closed records
    /// and their actions are listed while tabs are open.
    session_terminal: bool,
    /// Whether the sidebar is collapsed to its icons.
    rail_collapsed: bool,
    /// What the user chose for the palette.
    theme_choice: theme::ThemeChoice,
    /// Whether the window already tells this view when the system palette changes.
    watching_appearance: bool,
    /// Whether the host dialog's first field takes the keyboard on the next draw.
    focus_host_dialog: bool,
    /// The session row whose menu is open, and where it was opened.
    session_menu: Option<(SessionId, gpui::Point<gpui::Pixels>)>,
    /// Attempts a test scripted, kept so their answers can be inspected.
    #[cfg(test)]
    scripted: Vec<(SessionId, shellune_ssh::ScriptedSession)>,
}

/// The three credential fields of the remote form.
struct RemoteFields {
    password: Entity<TextField>,
    key_path: Entity<TextField>,
    passphrase: Entity<TextField>,
}

/// The fields of the host dialog, in form order.
struct HostFields {
    name: Entity<TextField>,
    address: Entity<TextField>,
    port: Entity<TextField>,
    user: Entity<TextField>,
    group: Entity<TextField>,
    identity: Entity<TextField>,
    secret: Entity<TextField>,
    startup: Entity<TextField>,
}

impl HostFields {
    fn get(&self, field: HostField) -> &Entity<TextField> {
        match field {
            HostField::Name => &self.name,
            HostField::Address => &self.address,
            HostField::Port => &self.port,
            HostField::User => &self.user,
            HostField::Group => &self.group,
            HostField::IdentityFile => &self.identity,
            HostField::Secret => &self.secret,
            HostField::StartupCommand => &self.startup,
        }
    }
}

impl ShelluneView {
    /// Builds the shell in its initial state, reading the saved hosts.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_workspace(cx, Workspace::new())
    }

    /// Builds the shell around an existing workspace, used by tests and by later
    /// tasks that restore state before the first frame.
    pub fn with_workspace(cx: &mut Context<Self>, workspace: Workspace) -> Self {
        Self::with_store(cx, workspace, default_store())
    }

    /// Builds the shell around an existing workspace and host store.
    ///
    /// The hosts section the caller passed decides where the records come from: a
    /// loading section means "read the store", anything else means the caller has
    /// already established the state and the store is only where saves will go.
    pub fn with_store(cx: &mut Context<Self>, workspace: Workspace, store: HostStore) -> Self {
        Self::with_store_and_trust(cx, workspace, store, default_trust())
    }

    /// Builds the shell around an existing workspace, host store and trust file.
    pub fn with_store_and_trust(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
        trust: TrustStore,
    ) -> Self {
        let placeholder = workspace.strings().shell_custom_placeholder;
        let search_placeholder = workspace.strings().hosts_search_placeholder;
        let strings = workspace.strings();
        let shell_path = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_browse(Browse::File, cx);
            field
        });
        let hosts_search = cx.new(|cx| TextField::new(search_placeholder, cx));

        // The credentials fields are created once and reused: a field that is
        // recreated per attempt would lose the caret on every keystroke that
        // redraws the panel.
        let password = cx.new(|cx| {
            let mut field = TextField::new(strings.auth_password, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        let key_path = cx.new(|cx| {
            let mut field = TextField::new(strings.auth_key, cx);
            field.set_browse(Browse::File, cx);
            field
        });
        let passphrase = cx.new(|cx| {
            let mut field = TextField::new(strings.auth_key_passphrase, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        let quick_address = cx.new(|cx| TextField::new(strings.host_field_address, cx));
        let quick_port = cx.new(|cx| {
            let mut field = TextField::new(strings.host_field_port, cx);
            field.set_text("22", cx);
            field
        });
        let quick_user = cx.new(|cx| TextField::new(strings.host_field_user, cx));
        let quick_password = cx.new(|cx| {
            let mut field = TextField::new(strings.auth_password, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });

        let mut view = Self {
            workspace,
            root_focus: cx.focus_handle(),
            destination_focus: Destination::ALL.iter().map(|_| cx.focus_handle()).collect(),
            panes: Vec::new(),
            active: None,
            local_error: None,
            shell_path,
            custom_shell_open: false,
            shell_menu_open: false,
            store,
            book: HostBook::new(),
            hosts: HostsPanel::new(),
            hosts_search,
            host_fields: None,
            import: ImportPanel::new(),
            import_path: None,
            remote: None,
            attempts: Vec::new(),
            form_session: None,
            quick_open: false,
            quick_error: None,
            quick_agent: false,
            quick_address,
            quick_port,
            quick_user,
            quick_password,
            pending_ephemeral: None,
            unsaved: Vec::new(),
            showing_connection_form: false,
            reconnect: None,
            remote_fields: RemoteFields {
                password,
                key_path,
                passphrase,
            },
            trust,
            trust_book: HostTrust::new(),
            trust_error: None,
            secrets: default_secrets(),
            files: FileViews::new(),
            transfers: TransferViews::new(),
            transfer_problem: None,
            closing: None,
            files_polling: false,
            files_dialog: None,
            files_field: None,
            palette: None,
            palette_query: String::new(),
            palette_selected: 0,
            palette_scroll: gpui::ScrollHandle::new(),
            tab_menu: None,
            session_menu: None,
            focus_host_dialog: false,
            rail_collapsed: false,
            theme_choice: theme::ThemeChoice::default(),
            watching_appearance: false,
            session_terminal: false,
            #[cfg(test)]
            scripted: Vec::new(),
        };
        view.bind_cleanup(cx);
        view.bind_shell_path(cx);
        view.bind_host_search(cx);
        view.bind_remote_fields(cx);
        view.restore_hosts(cx);
        view.restore_trust();
        view
    }

    /// Ends every session when the window that shows them closes (R7).
    ///
    /// Closing the window is the end of this application, so the shells and the
    /// connection threads it started must not outlive it. Each source is shut down
    /// through its own session, and nothing outside this shell is touched.
    fn bind_cleanup(&mut self, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        cx.on_window_closed(move |app| {
            let _ = view.update(app, |view, cx| view.close_all_sessions(cx));
        })
        .detach();
    }

    /// Reads the accepted server identities, or reports that they could not be read.
    ///
    /// A trust file that cannot be read is not an empty library: the shell keeps no
    /// records and says why, so an unreadable file can never silently turn into
    /// "ask the user again" for a host they already accepted.
    fn restore_trust(&mut self) {
        match self.trust.load() {
            Ok(trust) => {
                self.trust_book = trust;
                self.trust_error = None;
            }
            Err(error) => {
                self.trust_book = HostTrust::new();
                self.trust_error = Some(error.to_string());
            }
        }
    }

    /// Reads the saved hosts, or adopts the section the caller already built.
    ///
    /// A `Loading` section is one nobody has filled in yet - that is how a fresh
    /// workspace starts - so it means "this shell has to read the store". Anything else
    /// is a result a caller already established.
    fn restore_hosts(&mut self, cx: &mut Context<Self>) {
        if matches!(self.workspace.hosts().status(), SectionStatus::Loading) {
            self.reload_hosts(cx);
            return;
        }
        // A restored section is a real result from the caller; seeding the book with
        // it keeps the list and the records from disagreeing. Records that break the
        // invariants are dropped rather than rendered: the store is the only place
        // that can say what is really saved.
        self.book =
            HostBook::from_hosts(self.workspace.hosts().items().to_vec()).unwrap_or_default();
    }

    /// The saved records this shell last read or wrote.
    ///
    /// Read by the shell tests, and by the steps that record a real connection.
    #[allow(dead_code)]
    #[must_use]
    pub fn book(&self) -> &HostBook {
        &self.book
    }

    /// The host list views, the open form and the last message.
    #[allow(dead_code)]
    #[must_use]
    pub fn hosts_state(&self) -> &HostsPanel {
        &self.hosts
    }

    /// The import panel and what the last read of a configuration file showed.
    #[cfg(test)]
    pub fn import_state(&self) -> &ImportPanel {
        &self.import
    }

    /// Selects one host list view before the first frame, so a test can observe what
    /// that view draws on its own instead of what an earlier frame left behind.
    #[cfg(test)]
    pub fn select_host_view_for_test(&mut self, view: HostView, cx: &mut Context<Self>) {
        self.hosts.select_view(view);
        cx.notify();
    }

    /// The terminal on screen, if the selected session has one.
    ///
    /// This is the terminal the user is looking at, not "the only one": several
    /// sessions can be open, and the ones that are not selected keep their processes
    /// and their screens.
    #[cfg(test)]
    pub fn pane(&self) -> Option<&Entity<TerminalPane>> {
        self.active_pane()
    }

    /// The session whose terminal is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn active_session(&self) -> Option<SessionId> {
        self.active
    }

    /// The sessions that have a terminal open, in the order they were opened.
    #[must_use]
    pub fn open_sessions(&self) -> Vec<SessionId> {
        self.panes.iter().map(|open| open.tag.session).collect()
    }

    /// The terminal of one session, whether or not it is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn pane_of(&self, id: SessionId) -> Option<&Entity<TerminalPane>> {
        self.pane_for(id)
    }

    /// The sessions the tab strip shows, in the order they were opened.
    ///
    /// Every session that is not closed has a tab: one that is still connecting has no
    /// terminal yet and still needs one, because the tab is the only place that reports
    /// what the attempt is doing. A closed session is over and has none.
    fn tab_sessions(&self) -> Vec<SessionId> {
        self.workspace
            .sessions()
            .items()
            .iter()
            .filter(|session| !matches!(session.state(), SessionState::Closed))
            .map(Session::id)
            .collect()
    }

    /// The sessions the tab strip would draw, for the tests that check what is left
    /// after a close.
    ///
    /// Read instead of the rendered bounds on purpose: the test window keeps the debug
    /// bounds of every frame it has drawn, so an element that disappeared cannot be
    /// proven gone that way.
    #[cfg(test)]
    pub fn open_tabs(&self) -> Vec<SessionId> {
        self.tab_sessions()
    }

    fn pane_for(&self, id: SessionId) -> Option<&Entity<TerminalPane>> {
        self.panes
            .iter()
            .find(|open| open.tag.session == id)
            .map(|open| &open.pane)
    }

    fn active_pane(&self) -> Option<&Entity<TerminalPane>> {
        self.active.and_then(|id| self.pane_for(id))
    }

    /// Takes the keyboard to the terminal of one session, when a window has one.
    fn focus_pane(&self, id: SessionId, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_for(id) else {
            return;
        };
        let handle = pane.read(cx).focus_handle().clone();
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_view, window, _cx| window.focus(&handle));
        }
    }

    /// Adds a terminal for a session that just started and shows it.
    fn push_pane(&mut self, tag: EventTag, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
        self.attach_pane(tag, pane, true, cx);
    }

    /// Adds a terminal. `focus` is false when a background attempt finished while the
    /// user is looking at another session or form: the new terminal exists, but it
    /// does not take the screen or the keyboard.
    fn attach_pane(
        &mut self,
        tag: EventTag,
        pane: Entity<TerminalPane>,
        focus: bool,
        cx: &mut Context<Self>,
    ) {
        // The pane reports what its process does; the entry is observed so the session
        // list repeats the truth about that one session.
        cx.observe(&pane, |view, pane, cx| view.sync_pane(pane, cx))
            .detach();
        self.panes.push(OpenPane { tag, pane });
        if focus {
            self.active = Some(tag.session);
            self.session_terminal = true;
            self.showing_connection_form = false;
            self.focus_pane(tag.session, cx);
        }
        cx.notify();
    }

    /// Ends the process of one session and drops its terminal.
    ///
    /// Only that session is touched: every other terminal keeps its process and its
    /// screen, which is what makes closing one tab safe while the rest run.
    fn drop_pane(&mut self, id: SessionId, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.panes.iter().position(|open| open.tag.session == id) else {
            return false;
        };
        let open = self.panes.remove(index);
        open.pane.update(cx, |pane, cx| pane.close(cx));
        // The connection of this session ends with its terminal, so its file view goes
        // with it instead of keeping a path and a listing that nothing backs any more.
        self.end_files(id);
        if self.active == Some(id) {
            // The next tab takes over, or the previous one when this was the last: a
            // shell never ends up with no terminal selected while one is still open.
            self.active = self
                .panes
                .get(index)
                .or_else(|| self.panes.last())
                .map(|open| open.tag.session);
            if let Some(next) = self.active {
                self.focus_pane(next, cx);
            }
        }
        cx.notify();
        true
    }

    /// Shows the terminal of one session, from wherever the user is.
    ///
    /// Local and remote sessions share this workspace. Selecting one does not send
    /// the user to a different page because of its kind. A tab that is still
    /// connecting focuses that attempt's own form and leaves every other attempt
    /// running.
    pub fn activate(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.pane_for(id).is_none() {
            if self.attempt_index(id).is_some() {
                self.focus_attempt(id, cx);
            }
            return;
        }
        self.showing_connection_form = false;
        self.active = Some(id);
        self.session_terminal = true;
        // Hosts and transfers are context panels. Coming back from one of them lands
        // on the workspace, which shows whichever session was selected.
        if !matches!(
            self.workspace.destination(),
            Destination::LocalShells | Destination::Sessions
        ) {
            self.workspace.select(Destination::LocalShells);
            self.workspace
                .set_context(shellune_core::PanelSection::None);
        }
        self.focus_pane(id, cx);
        cx.notify();
    }

    /// Shows a session's terminal and gives it the keyboard through `window`.
    ///
    /// [`Self::activate`] focuses through the application's active window, which is
    /// already borrowed while a key action runs, so from a shortcut that focus would be
    /// lost and the next shortcut would land on the terminal that was just hidden.
    pub fn activate_in(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(id, cx);
        if self.active == Some(id)
            && let Some(pane) = self.pane_for(id)
        {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
        }
    }

    /// Shows the tab at `index`, counting from the left; nothing happens past the end.
    pub fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tab_sessions().get(index).copied() {
            self.activate_in(id, window, cx);
        }
    }

    /// Shows the tab `delta` places from the current one, wrapping at both ends.
    pub fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.tab_sessions();
        if tabs.is_empty() {
            return;
        }
        let current = self
            .active
            .and_then(|id| tabs.iter().position(|tab| *tab == id))
            .unwrap_or(0);
        // A tab that is still connecting has no terminal to show, so it is stepped over
        // instead of stopping the cycle on it.
        let mut next = current;
        for _ in 0..tabs.len() {
            next = crate::app::palette::step(next, delta, tabs.len());
            if self.pane_for(tabs[next]).is_some() {
                self.activate_in(tabs[next], window, cx);
                return;
            }
        }
    }

    /// Closes one session: its process, its terminal and its tab (R7).
    ///
    /// Nothing else is closed. An attempt that has not become a terminal yet is ended
    /// the same way, so a tab can be closed while its connection is still being made,
    /// and the other sessions keep answering.
    pub fn close_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.discard_attempt(id);
        self.unsaved.retain(|(session, _)| *session != id);
        self.drop_pane(id, cx);
        self.workspace.session_closed(id);
        // A connecting attempt has no terminal, so closing it does not move the
        // selection by itself. Point it at a session that is still open.
        if self.active == Some(id) && self.pane_for(id).is_none() {
            self.active = self
                .panes
                .last()
                .map(|open| open.tag.session)
                .or_else(|| self.attempts.first().map(|attempt| attempt.id));
        }
        // A form that was showing this session can no longer be live: it goes away with
        // the session it belonged to, and takes the secrets it held with it.
        if self.form_session == Some(id) {
            self.form_session = None;
            if let Some(panel) = self.remote.as_mut() {
                panel.clear_secrets();
            }
            self.remote = None;
            self.showing_connection_form = false;
        }
        if self.reconnect == Some(id) {
            self.reconnect = None;
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Transfers (R10, R17)
    // -----------------------------------------------------------------------

    /// Asks for the local file to upload into the directory on screen (R10).
    ///
    /// The question names the directory, so where the file would land is visible before
    /// anything is sent; the field is where the local path is typed.
    fn files_ask_upload(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(directory) = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned)
        else {
            return;
        };
        let placeholder = self.strings().transfers_upload_placeholder;
        let field = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_browse(Browse::File, cx);
            field
        });
        self.open_files_dialog(
            FilesDialog::Upload {
                session: id,
                directory,
                text: String::new(),
                problem: None,
            },
            field,
            cx,
        );
    }

    /// Asks where one remote entry should be downloaded to (R10).
    ///
    /// The folder starts at this account's home directory: a real place on this machine
    /// that the user can correct, instead of an empty field with no hint.
    fn files_ask_download(&mut self, id: SessionId, name: &str, cx: &mut Context<Self>) {
        let Some(directory) = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned)
        else {
            return;
        };
        let remote = join_path(&directory, name);
        let placeholder = self.strings().transfers_download_placeholder;
        let suggested = default_download_directory();
        let field = cx.new(|cx| {
            let mut field = TextField::with_text(placeholder, &suggested, cx);
            field.set_browse(Browse::Directory, cx);
            field
        });
        self.open_files_dialog(
            FilesDialog::Download {
                session: id,
                remote,
                text: suggested,
                problem: None,
            },
            field,
            cx,
        );
    }

    /// Starts the upload the question described, or says why it cannot start.
    fn start_upload(&mut self, id: SessionId, local: &str, cx: &mut Context<Self>) {
        let directory = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned);
        let started = self.transfers.upload(id, local, directory.as_deref());
        self.finish_transfer_question(id, started, cx);
    }

    /// Starts the download the question described, or says why it cannot start.
    fn start_download(
        &mut self,
        id: SessionId,
        remote: &str,
        folder: &str,
        cx: &mut Context<Self>,
    ) {
        let started = self.transfers.download(id, remote, folder);
        self.finish_transfer_question(id, started, cx);
    }

    /// Closes a transfer question and refreshes the window, or keeps it open with the
    /// reason it could not start.
    fn finish_transfer_question(
        &mut self,
        id: SessionId,
        started: Result<shellune_core::TransferId, TransferProblem>,
        cx: &mut Context<Self>,
    ) {
        match started {
            Ok(_) => {
                self.files_dialog = None;
                self.files_field = None;
                self.transfer_problem = None;
                self.watch_files(cx);
            }
            Err(problem) => {
                // A connection that cannot take the command is not a mistake in what was
                // typed, so the field keeps no mark for it: the list reports the failed
                // task with the connection problem it is.
                let field_problem = match problem {
                    TransferProblem::SourceNamesNoFile => Some(PathProblem::NoName),
                    TransferProblem::DirectoryUnknown => Some(PathProblem::Empty),
                    TransferProblem::CommandRefused => None,
                };
                match (field_problem, self.files_dialog.as_mut()) {
                    (Some(field_problem), Some(dialog)) => dialog.fail_path(field_problem),
                    _ => {
                        self.files_dialog = None;
                        self.files_field = None;
                        // Nothing was typed wrongly here: the connection could not take
                        // the command, so the reason is reported beside the list.
                        self.transfer_problem = Some((id, problem));
                    }
                }
            }
        }
        self.sync_transfer_section();
        cx.notify();
    }

    /// Cancels one transfer (R10).
    fn transfer_cancel(&mut self, id: shellune_core::TransferId, cx: &mut Context<Self>) {
        self.transfers.cancel(id);
        self.sync_transfer_section();
        cx.notify();
    }

    /// Starts a new task for a transfer that failed (R17).
    ///
    /// It is a new task with a new identifier, never a continuation: the bytes begin
    /// again at zero, and the failed attempt stays in the list beside it.
    fn transfer_retry(&mut self, id: shellune_core::TransferId, cx: &mut Context<Self>) {
        self.transfers.retry(id);
        self.watch_files(cx);
        self.sync_transfer_section();
        cx.notify();
    }

    /// Answers the question about an existing target (R10).
    fn transfer_decide(
        &mut self,
        id: shellune_core::TransferId,
        choice: shellune_core::OverwriteChoice,
        cx: &mut Context<Self>,
    ) {
        self.transfers.decide(id, choice);
        self.sync_transfer_section();
        cx.notify();
    }

    /// Copies what the transfers really are into the section the transfers page lists.
    ///
    /// The page and the panel therefore read one source: a task the worker has not
    /// reported is queued on both, and a completed one is never shown as running.
    fn sync_transfer_section(&mut self) {
        let items = self.transfers.transfers().to_vec();
        self.workspace.replace_transfers(items);
    }

    /// Asks before closing a session that is moving files (R10).
    ///
    /// Closing a session cancels the transfers that belong to it, so that consequence is
    /// stated first, with how many tasks would stop. A session with nothing in flight
    /// closes at once, because there is nothing to warn about.
    pub fn request_close_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.transfers.active_count(id) > 0 {
            self.closing = Some(id);
            cx.notify();
            return;
        }
        self.close_session(id, cx);
    }

    /// Keeps the session open and its transfers running.
    fn cancel_close_session(&mut self, cx: &mut Context<Self>) {
        self.closing = None;
        cx.notify();
    }

    /// Closes the session the warning is about (R10).
    ///
    /// Only the transfers of this session are cancelled: the tasks of every other
    /// session keep running, because closing one tab says nothing about another.
    fn confirm_close_session(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.closing.take() else {
            return;
        };
        self.close_session(id, cx);
    }

    /// The question shown before a session with active transfers is closed (R10).
    fn close_transfer_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let id = self.closing?;
        let active = self.transfers.active_count(id);
        if active == 0 {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let target = self
            .workspace
            .session(id)
            .map_or_else(String::new, |session| session.title().to_owned());
        Some(
            components::banner(Tone::Danger, metrics)
                .flex_row()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .mx(metrics.scale(spacing::SNUG))
                .mt(metrics.scale(spacing::SNUG))
                .w_auto()
                .debug_selector(|| "session.close.question".to_owned())
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .debug_selector(|| "session.close.question.text".to_owned())
                        .child(SharedString::from(format!(
                            "{}: {target} · {active}",
                            strings.transfers_close_question
                        ))),
                )
                .child(self.text_button(
                    SharedString::from("session.close.cancel"),
                    strings.transfers_close_cancel,
                    Variant::Secondary,
                    cx,
                    |view, cx| view.cancel_close_session(cx),
                ))
                .child(self.text_button(
                    SharedString::from("session.close.confirm"),
                    strings.transfers_close_confirm,
                    Variant::Danger,
                    cx,
                    move |view, cx| view.confirm_close_session(cx),
                )),
        )
    }

    /// Closes every session, which is what leaving the application does (R7).
    ///
    /// Each source is shut down through its own session, so the processes and the
    /// connection threads end with the window instead of outliving it.
    pub fn close_all_sessions(&mut self, cx: &mut Context<Self>) {
        let ids = self.open_sessions();
        for id in ids {
            self.close_session(id, cx);
        }
        let pending: Vec<SessionId> = self.attempts.iter().map(|attempt| attempt.id).collect();
        for id in pending {
            self.discard_attempt(id);
            self.workspace.session_closed(id);
        }
        self.remote = None;
        self.form_session = None;
        self.reconnect = None;
        self.showing_connection_form = false;
        self.unsaved.clear();
    }

    /// Starts another process for a session that ended (R7).
    ///
    /// The tab, its name and its target stay; the process, the terminal and the event
    /// generation are new. Nothing typed into the old process is sent again: the new
    /// terminal starts empty and only what the user types next reaches the new shell.
    pub fn reconnect_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(session) = self.workspace.session(id) else {
            return;
        };
        if !may_reconnect(session) {
            return;
        }
        let kind = session.kind().clone();
        match kind {
            SessionKind::Local { shell } => {
                let Some(program) = shell else {
                    let reason = self.strings().session_reconnect_no_shell.to_owned();
                    self.refuse_reconnect(id, reason, cx);
                    return;
                };
                // The old terminal is over: its process is gone, and the generation
                // moves on so a late event from it can no longer reach the terminal
                // that replaces it.
                self.drop_pane(id, cx);
                if !self.workspace.session_reconnecting(id) {
                    return;
                }
                let name = self
                    .workspace
                    .shells()
                    .iter()
                    .find(|known| known.program() == program)
                    .map_or_else(|| program.clone(), |known| known.name().to_owned());
                self.start_local_pane(id, LocalShell::new(name, program), cx);
            }
            SessionKind::Remote { host } => {
                let Some(host) = self.book.host(host).cloned() else {
                    let reason = self.strings().session_reconnect_missing_host.to_owned();
                    self.refuse_reconnect(id, reason, cx);
                    return;
                };
                // A remote session needs credentials again: the password and the
                // passphrase were handed to the previous attempt once and are gone. The
                // form therefore opens with everything that is not a secret already
                // filled in, and the attempt it starts reuses this session's id.
                //
                // Nothing is connecting yet, so the record keeps the reason it ended
                // until the form really starts a new attempt: a tab that said
                // "connecting" over an empty form would claim a connection nobody made.
                self.drop_pane(id, cx);
                self.stash_focused_panel();
                self.discard_attempt(id);
                self.reconnect = Some(id);
                self.form_session = Some(id);
                self.remote = Some(RemotePanel::new(&host));
                self.showing_connection_form = true;
                self.sync_remote_fields(cx);
                // A host that remembers how to sign in reconnects in one click too.
                self.connect_when_ready(&host, cx);
                cx.notify();
            }
        }
    }

    /// What the credential store holds for a host, for tests.
    #[cfg(test)]
    pub fn saved_secret_for_test(&self, id: HostId) -> Option<String> {
        self.secrets
            .load(id)
            .ok()
            .flatten()
            .map(|secret| secret.expose().to_owned())
    }

    /// Whether the session bar offers the reconnect button for this session.
    #[cfg(test)]
    pub fn offers_reconnect(&self, id: SessionId) -> bool {
        self.workspace.session(id).is_some_and(may_reconnect)
    }

    /// Records that a reconnect could not start, and why.
    ///
    /// Recorded as [SessionFailure::ReconnectUnavailable] rather than as a transport
    /// failure: nothing about these causes changes by pressing reconnect again, so the
    /// session must stop offering it and name the step that does work - closing it (R12).
    fn refuse_reconnect(&mut self, id: SessionId, reason: String, cx: &mut Context<Self>) {
        // The session has already ended, and a transition to "ended" again would be
        // refused exactly where the refusal matters, so the reason is restated instead:
        // the session stays ended, and it now also says that no attempt can bring it back.
        self.workspace
            .session_end_restated(id, SessionFailure::ReconnectUnavailable { reason });
        cx.notify();
    }

    /// Whether the header's shell menu is open, for a test.
    #[cfg(test)]
    pub fn shell_menu_open(&self) -> bool {
        self.shell_menu_open
    }

    /// Whether the free-text shell path field is on screen.
    #[cfg(test)]
    pub fn custom_shell_open(&self) -> bool {
        self.custom_shell_open
    }

    /// What the free-text shell path field holds.
    #[cfg(test)]
    pub fn shell_path_text(&self, cx: &gpui::App) -> String {
        self.shell_path.read(cx).text().to_owned()
    }

    /// The current workspace state.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Focus handle of the shell root, which owns the key context.
    pub fn root_focus(&self) -> &FocusHandle {
        &self.root_focus
    }

    /// Focus handle of one navigation entry.
    pub fn destination_focus(&self, destination: Destination) -> &FocusHandle {
        &self.destination_focus[destination.index()]
    }

    fn strings(&self) -> &'static Strings {
        self.workspace.strings()
    }

    fn metrics(&self) -> Metrics {
        self.workspace.metrics()
    }

    fn primary_label(&self, destination: Destination, strings: &Strings) -> &'static str {
        match destination {
            Destination::LocalShells => strings.primary_local,
            Destination::Hosts => strings.primary_hosts,
            Destination::Sessions => strings.primary_sessions,
            Destination::Transfers => strings.primary_transfers,
        }
    }

    /// Selects a destination, moving keyboard focus with it.
    ///
    /// Returning to the workspace gives the keyboard back to the session that was
    /// on screen. A host list that is still loading does not get in the way: the
    /// destination changes either way.
    fn select(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        if destination == Destination::Sessions {
            // The page itself was asked for: it lists the sessions.
            self.session_terminal = false;
            self.showing_connection_form = false;
        }
        if destination == Destination::LocalShells {
            self.showing_connection_form = false;
        }
        self.workspace.select(destination);
        self.workspace.set_context(match destination {
            Destination::Hosts => shellune_core::PanelSection::Hosts,
            Destination::Transfers => shellune_core::PanelSection::Transfers,
            _ => shellune_core::PanelSection::None,
        });
        if self.shows_terminal(destination)
            && let Some(pane) = self.active_pane()
        {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
            cx.notify();
            return;
        }
        let handle = self.destination_focus(destination).clone();
        window.focus(&handle);
        cx.notify();
    }

    fn request_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.workspace.destination() {
            // The sessions page lists local shells too, so its main button opens one;
            // the new terminal is shown right there.
            Destination::LocalShells | Destination::Sessions => {
                self.new_local_shell(window, cx);
            }
            Destination::Hosts => {
                // A destination whose records could not be read offers reading them
                // again; every other state of it offers a new host.
                if self.workspace.state(Destination::Hosts) == SectionState::Failed {
                    self.reload_hosts(cx);
                } else {
                    self.open_create_host(cx);
                }
            }
            // A transfer starts from the file view of a connection, so the page's
            // button goes there: to an open file view when a connection can browse,
            // and to the saved hosts when none can yet. Nothing is started here.
            Destination::Transfers => match self.browsable_session() {
                Some(id) => {
                    self.activate(id, cx);
                    if !self.files.is_open(id) {
                        self.toggle_files(id, cx);
                    }
                }
                None => self.select(Destination::Hosts, window, cx),
            },
        }
        cx.notify();
    }

    /// Opens the default shell in a new tab, from the header, the tab strip or the
    /// shortcut.
    fn new_local_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell_menu_open = false;
        self.start_from_picker(window, cx);
    }

    /// Connects the free-text shell field to this view, so Enter starts a shell.
    fn bind_shell_path(&mut self, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        self.shell_path.update(cx, |field, _cx| {
            field.on_submit(move |text, window, app| {
                view.update(app, |view, cx| view.start_custom_shell(text, window, cx))
                    .unwrap_or(false)
            });
        });
    }

    // -----------------------------------------------------------------------
    // Host management (R3)
    // -----------------------------------------------------------------------

    /// Mirrors the search field into the panel, so the list filters as the user types.
    fn bind_host_search(&mut self, cx: &mut Context<Self>) {
        let search = self.hosts_search.clone();
        cx.observe(&search, |view, field, cx| {
            let phrase = field.read(cx).text().to_owned();
            if view.hosts.search() != phrase {
                view.hosts.set_search(phrase);
                cx.notify();
            }
        })
        .detach();
    }

    /// Mirrors one form field into the draft and lets Enter save the form.
    fn bind_host_field(&mut self, field: HostField, cx: &mut Context<Self>) {
        let Some(entity) = self
            .host_fields
            .as_ref()
            .map(|fields| fields.get(field).clone())
        else {
            return;
        };

        let view = cx.entity().downgrade();
        entity.update(cx, |text_field, _cx| {
            text_field.on_submit(move |_text, _window, app| {
                view.update(app, |view, cx| {
                    let _ = view.save_host(cx);
                    // The field keeps its text: when the save was refused, the reason
                    // points at what the user typed and must still be there.
                    false
                })
                .unwrap_or(false)
            });
        });

        cx.observe(&entity, move |view, text_field, cx| {
            let text = text_field.read(cx).text().to_owned();
            if let Some(draft) = view.hosts.draft_mut() {
                draft.set_text(field, text);
            }
            cx.notify();
        })
        .detach();
    }

    /// Reads the host file again and reports what it really holds.
    fn reload_hosts(&mut self, cx: &mut Context<Self>) {
        match self.store.load() {
            Ok(book) => {
                self.book = book;
                self.hosts.clear_message();
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
            }
            Err(error) => {
                // A file that cannot be read is not an empty list: the section keeps
                // the reason the storage layer produced and holds no records, so no
                // host is shown that the file does not really contain.
                self.book = HostBook::new();
                self.host_fields = None;
                self.workspace
                    .fail_hosts(Failure::new(error.to_string(), Recovery::Retry));
            }
        }
        cx.notify();
    }

    /// Opens an empty form for a new host.
    fn open_create_host(&mut self, cx: &mut Context<Self>) {
        self.hosts.open_create();
        self.open_host_fields(cx);
    }

    /// Opens the form filled with a saved host.
    fn open_edit_host(&mut self, host: &HostConfig, cx: &mut Context<Self>) {
        self.hosts.open_edit(host);
        self.open_host_fields(cx);
    }

    fn open_host_fields(&mut self, cx: &mut Context<Self>) {
        let draft = self
            .hosts
            .draft()
            .cloned()
            .unwrap_or_else(HostDraft::create);
        let strings = self.strings();
        let name = cx.new(|cx| {
            TextField::with_text(strings.host_field_name, draft.text(HostField::Name), cx)
        });
        let address = cx.new(|cx| {
            TextField::with_text(
                strings.host_field_address,
                draft.text(HostField::Address),
                cx,
            )
        });
        let port = cx.new(|cx| {
            TextField::with_text(strings.host_field_port, draft.text(HostField::Port), cx)
        });
        let user = cx.new(|cx| {
            TextField::with_text(strings.host_field_user, draft.text(HostField::User), cx)
        });
        let group = cx.new(|cx| {
            TextField::with_text(strings.host_field_group, draft.text(HostField::Group), cx)
        });
        let identity = cx.new(|cx| {
            let mut field = TextField::with_text(
                strings.host_field_identity,
                draft.text(HostField::IdentityFile),
                cx,
            );
            field.set_browse(Browse::File, cx);
            field
        });
        let secret_placeholder = if draft.has_saved_secret() {
            strings.host_secret_saved_placeholder
        } else if draft.auth().needs_passphrase() {
            strings.host_secret_passphrase
        } else {
            strings.auth_password
        };
        let secret = cx.new(|cx| {
            let mut field = TextField::new(secret_placeholder, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        let startup = cx.new(|cx| {
            TextField::with_text(
                strings.host_startup_placeholder,
                draft.text(HostField::StartupCommand),
                cx,
            )
        });
        self.host_fields = Some(HostFields {
            name,
            address,
            port,
            user,
            group,
            identity,
            secret,
            startup,
        });
        for field in HostField::ALL {
            self.bind_host_field(field, cx);
        }
        self.focus_host_dialog = true;
        cx.notify();
    }

    /// Closes the form without touching the saved records.
    fn close_host_editor(&mut self, cx: &mut Context<Self>) {
        self.hosts.close_editor();
        self.host_fields = None;
        cx.notify();
    }

    /// Validates the form and stores it when the domain accepts it.
    ///
    /// Returns the id of the host that was saved, or `None` when the dialog stays open
    /// with the reason it was refused.
    fn save_host(&mut self, cx: &mut Context<Self>) -> Option<HostId> {
        let draft = self.hosts.draft().cloned()?;
        let saved = draft.editing().and_then(|id| self.book.host(id)).cloned();
        let edit = match draft.build(saved.as_ref()) {
            Ok(edit) => edit,
            Err(reason) => {
                // The form stays open with what the user typed, and the reason names
                // the field the domain refused.
                self.hosts.set_message(reason, true);
                cx.notify();
                return None;
            }
        };

        let mut next = self.book.clone();
        if let Err(error) = next.apply(&edit) {
            self.hosts.set_message(error.to_string(), true);
            cx.notify();
            return None;
        }
        if !self.store_host_secret(&draft, edit.host(), saved.as_ref(), cx) {
            return None;
        }
        self.commit_hosts(next, self.strings().hosts_saved, cx);
        // The dialog closes only when the file was really written.
        self.host_fields.is_none().then(|| edit.id())
    }

    /// Puts the secret typed in the form into the system store, or takes it out.
    ///
    /// Runs before the record is written: a host whose file says "remembered" must have
    /// its secret in the store. Returns whether saving the record may go on; when the
    /// store refuses, the dialog stays open with the reason.
    fn store_host_secret(
        &mut self,
        draft: &HostDraft,
        host: &HostConfig,
        saved: Option<&HostConfig>,
        cx: &mut Context<Self>,
    ) -> bool {
        let strings = self.strings();
        if host.remembers_secret() {
            // Nothing typed keeps what is saved: the draft only remembers when there is
            // something to remember.
            if let Some(secret) = draft.typed_secret() {
                let secret = shellune_core::SecretString::new(secret);
                if let Err(error) = self.secrets.save(host.id(), &secret) {
                    self.hosts
                        .set_message(format!("{}: {error}", strings.host_secret_failed), true);
                    cx.notify();
                    return false;
                }
            }
        } else if saved.is_some_and(HostConfig::remembers_secret) {
            // Turning remembering off removes the secret; a store that cannot be reached
            // leaves an entry nobody reads, which is not a reason to keep the old flag.
            let _ = self.secrets.forget(host.id());
        }
        if let Some(draft) = self.hosts.draft_mut() {
            draft.clear_secret();
        }
        true
    }

    /// Saves the records, and only then adopts them as the current state.
    fn commit_hosts(&mut self, next: HostBook, message: &str, cx: &mut Context<Self>) {
        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                self.hosts.close_editor();
                self.host_fields = None;
                self.hosts.set_message(message.to_owned(), false);
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
            }
            // The records on disk did not change, so neither does the list: the
            // message carries the reason the write produced.
            Err(error) => self.hosts.set_message(error.to_string(), true),
        }
        cx.notify();
    }

    /// Asks the deletion question for one host.
    fn ask_delete_host(&mut self, id: HostId, cx: &mut Context<Self>) {
        self.host_fields = None;
        self.hosts.ask_delete(id);
        cx.notify();
    }

    /// Drops the deletion question without touching anything.
    fn cancel_delete_host(&mut self, cx: &mut Context<Self>) {
        self.hosts.cancel_delete();
        cx.notify();
    }

    /// Deletes the host the question is about, once the answer is stored.
    fn confirm_delete_host(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.hosts.take_delete() else {
            return;
        };
        let mut next = self.book.clone();
        let removed = match next.remove(id) {
            Ok(removed) => removed,
            Err(error) => {
                self.hosts.set_message(error.to_string(), true);
                cx.notify();
                return;
            }
        };
        // A deleted host's secret has nobody left to use it.
        if removed.remembers_secret() {
            let _ = self.secrets.forget(id);
        }
        self.commit_hosts(next, self.strings().hosts_deleted, cx);
    }

    // -----------------------------------------------------------------------
    // SSH configuration import (R5)
    // -----------------------------------------------------------------------

    /// Mirrors the configuration path field into the panel and lets Enter preview it.
    fn bind_import_path(&mut self, cx: &mut Context<Self>) {
        let Some(field) = self.import_path.clone() else {
            return;
        };

        let view = cx.entity().downgrade();
        field.update(cx, |text_field, _cx| {
            text_field.on_submit(move |_text, _window, app| {
                view.update(app, |view, cx| {
                    view.preview_import(cx);
                    false
                })
                .unwrap_or(false)
            });
        });

        cx.observe(&field, |view, text_field, cx| {
            let text = text_field.read(cx).text().to_owned();
            // A different path is a different question, so the previous preview goes
            // away instead of being applied to a file nobody looked at.
            view.import.set_path(text);
            cx.notify();
        })
        .detach();
    }

    /// Opens the import panel on the file this platform's ssh would read.
    ///
    /// A second click does nothing: the panel already shows a reading of a file, and
    /// replacing it would throw away a preview the user may be about to apply.
    fn open_import(&mut self, cx: &mut Context<Self>) {
        if self.import.is_open() {
            return;
        }
        let path = match shellune_storage::default_config_path() {
            Some(path) => path.display().to_string(),
            None => shellune_storage::DEFAULT_CONFIG_TEXT.to_owned(),
        };
        self.open_import_at(path, cx);
    }

    /// Opens the import panel on a named file, through the same path a click takes.
    #[cfg(test)]
    pub fn open_import_for_test(&mut self, path: &str, cx: &mut Context<Self>) {
        self.open_import_at(path.to_owned(), cx);
    }

    fn open_import_at(&mut self, path: String, cx: &mut Context<Self>) {
        self.import.open(path);
        self.hosts.close_editor();
        self.host_fields = None;
        self.hosts.clear_message();
        let placeholder = self.strings().import_path_placeholder;
        let text = self.import.path().to_owned();
        self.import_path = Some(cx.new(|cx| {
            let mut field = TextField::with_text(placeholder, text, cx);
            field.set_browse(Browse::File, cx);
            field
        }));
        self.bind_import_path(cx);
        cx.notify();
    }

    /// Closes the import panel without touching the saved hosts.
    fn close_import(&mut self, cx: &mut Context<Self>) {
        self.import.close();
        self.import_path = None;
        cx.notify();
    }

    /// Reads the named file and shows what importing it would do.
    ///
    /// A failure keeps the reason the importer produced, including the line that
    /// could not be parsed, and leaves the saved hosts alone.
    fn preview_import(&mut self, cx: &mut Context<Self>) {
        let path = self.import.path().trim().to_owned();
        if path.is_empty() {
            self.import.set_error(self.strings().import_path_required);
            cx.notify();
            return;
        }
        match ImportPreview::from_file(&path) {
            Ok(preview) => self.import.set_preview(preview),
            Err(error) => self.import.set_error(error.to_string()),
        }
        cx.notify();
    }

    /// Saves the hosts the preview offers, and says what it did not save.
    fn apply_import(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.import.preview().cloned() else {
            return;
        };
        let plan = preview.plan(&self.book);
        let imported = plan.len();
        // A host whose name was already saved is not imported at all, so it must not
        // be counted as one that was imported and needs a look.
        let review = preview
            .hosts()
            .iter()
            .filter(|host| host.outcome() == ImportOutcome::NeedsReview && host.is_importable())
            .filter(|host| {
                !plan
                    .skipped()
                    .iter()
                    .any(|skipped| skipped.alias() == host.alias())
            })
            .count();
        let skipped = plan.skipped().len();

        let mut next = self.book.clone();
        if let Err(error) = plan.apply(&mut next) {
            self.import.set_apply_error(error.to_string());
            cx.notify();
            return;
        }

        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
                self.hosts.set_message(
                    import_message(self.strings(), imported, review, skipped),
                    false,
                );
                self.import.close();
                self.import_path = None;
            }
            // The file did not change, so the preview stays: the same hosts can be
            // applied again once the write is possible.
            Err(error) => self.import.set_apply_error(error.to_string()),
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Remote connections (R4, R6)
    // -----------------------------------------------------------------------

    /// Mirrors the three credential fields into the open form.
    fn bind_remote_fields(&mut self, cx: &mut Context<Self>) {
        let password = self.remote_fields.password.clone();
        cx.observe(&password, |view, field, cx| {
            let text = field.read(cx).text().to_owned();
            if let Some(panel) = view.remote.as_mut() {
                panel.set_password(text);
            }
            cx.notify();
        })
        .detach();

        let key_path = self.remote_fields.key_path.clone();
        cx.observe(&key_path, |view, field, cx| {
            let text = field.read(cx).text().to_owned();
            if let Some(panel) = view.remote.as_mut() {
                panel.set_key_path(text);
            }
            cx.notify();
        })
        .detach();

        let passphrase = self.remote_fields.passphrase.clone();
        cx.observe(&passphrase, |view, field, cx| {
            let text = field.read(cx).text().to_owned();
            if let Some(panel) = view.remote.as_mut() {
                panel.set_passphrase(text);
            }
            cx.notify();
        })
        .detach();
    }

    /// Copies the form's values into the fields, for a form that just opened.
    fn sync_remote_fields(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.remote.as_ref() else {
            return;
        };
        let password = panel.password().to_owned();
        let key_path = panel.key_path().to_owned();
        let passphrase = panel.passphrase().to_owned();
        let password_field = self.remote_fields.password.clone();
        let key_field = self.remote_fields.key_path.clone();
        let passphrase_field = self.remote_fields.passphrase.clone();
        // The fields are emptied for the new attempt: a secret typed for the previous
        // host must not travel to this one.
        password_field.update(cx, |field, cx| field.set_text(password, cx));
        key_field.update(cx, |field, cx| field.set_text(key_path, cx));
        passphrase_field.update(cx, |field, cx| field.set_text(passphrase, cx));
    }

    /// Index of one attempt.
    fn attempt_index(&self, id: SessionId) -> Option<usize> {
        self.attempts.iter().position(|attempt| attempt.id == id)
    }

    /// The form of one attempt: the screen's form when it is focused, otherwise the
    /// copy kept on the attempt.
    fn panel_for(&self, id: SessionId) -> Option<&RemotePanel> {
        if self.form_session == Some(id) {
            return self.remote.as_ref();
        }
        self.attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .and_then(|attempt| attempt.panel.as_ref())
    }

    /// The form of one attempt, for an update that belongs to that attempt only.
    fn panel_for_mut(&mut self, id: SessionId) -> Option<&mut RemotePanel> {
        if self.form_session == Some(id) {
            return self.remote.as_mut();
        }
        self.attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .and_then(|attempt| attempt.panel.as_mut())
    }

    /// The transport of one attempt.
    fn transport_mut(&mut self, id: SessionId) -> Option<&mut RemoteSession> {
        self.attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .and_then(|attempt| attempt.session.as_mut())
    }

    /// Takes the transport of one attempt and drops that attempt.
    ///
    /// The focused form stays in [`Self::remote`]. A parked form is dropped with the
    /// attempt: the caller already copied anything it still needs, such as the host.
    /// A failure uses [`Self::close_attempt_transport`] instead, so the parked form
    /// can stay and show why that one attempt ended.
    fn take_transport(&mut self, id: SessionId) -> Option<RemoteSession> {
        let index = self.attempt_index(id)?;
        let mut attempt = self.attempts.remove(index);
        if self.form_session == Some(id) && self.remote.is_none() {
            self.remote = attempt.panel.take();
        }
        attempt.session.take()
    }

    /// Closes one attempt's transport and forgets it. Other attempts stay.
    fn discard_attempt(&mut self, id: SessionId) {
        if let Some(mut session) = self.take_transport(id) {
            session.close();
        }
        #[cfg(test)]
        self.scripted.retain(|(session, _)| *session != id);
    }

    /// Closes one attempt's transport after it failed, and keeps its form.
    ///
    /// The focused form stays on screen. A background form stays on its attempt, so
    /// the other-connections strip can still name it and the user can still cancel
    /// it. Neither path touches any other attempt.
    fn close_attempt_transport(&mut self, id: SessionId) {
        if self.form_session == Some(id) {
            if let Some(mut session) = self.take_transport(id) {
                session.close();
            }
        } else if let Some(index) = self.attempt_index(id) {
            if let Some(mut session) = self.attempts[index].session.take() {
                session.close();
            }
            self.attempts[index].watching = false;
        }
        #[cfg(test)]
        self.scripted.retain(|(session, _)| *session != id);
    }

    /// Puts the focused form back onto its attempt so another form can take the screen.
    fn stash_focused_panel(&mut self) {
        let Some(id) = self.form_session else {
            return;
        };
        let Some(index) = self.attempt_index(id) else {
            return;
        };
        if let Some(panel) = self.remote.take() {
            self.attempts[index].panel = Some(panel);
        }
    }

    /// Shows one attempt's own form without touching any other attempt.
    fn focus_attempt(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.form_session != Some(id) {
            self.stash_focused_panel();
            let Some(index) = self.attempt_index(id) else {
                cx.notify();
                return;
            };
            self.remote = self.attempts[index].panel.take();
            self.form_session = Some(id);
        }
        self.active = Some(id);
        self.showing_connection_form = true;
        self.session_terminal = true;
        if !matches!(
            self.workspace.destination(),
            Destination::LocalShells | Destination::Sessions | Destination::Hosts
        ) {
            self.workspace.select(Destination::LocalShells);
        }
        self.sync_remote_fields(cx);
        cx.notify();
    }

    /// Opens the quick-connection form on the workspace the user is already in.
    fn open_quick_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_open = true;
        self.quick_error = None;
        self.shell_menu_open = false;
        if !matches!(
            self.workspace.destination(),
            Destination::LocalShells | Destination::Sessions
        ) {
            self.select(Destination::LocalShells, window, cx);
        }
        let handle = self.quick_address.read(cx).focus_handle().clone();
        window.focus(&handle);
        cx.notify();
    }

    /// Builds an unsaved host from the quick-connection form and starts it.
    fn start_quick_connect(&mut self, cx: &mut Context<Self>) {
        if self.prepare_quick_connection(cx) {
            self.quick_open = false;
            self.connect_remote(cx);
        }
    }

    /// Fills the connection form from the quick-connection fields.
    ///
    /// Nothing is written to the host file. Missing fields stay on the form with a
    /// reason, and what was typed is kept.
    pub(crate) fn prepare_quick_connection(&mut self, cx: &mut Context<Self>) -> bool {
        let strings = self.strings();
        let address = self.quick_address.read(cx).text().trim().to_owned();
        let port_text = self.quick_port.read(cx).text().trim().to_owned();
        let user = self.quick_user.read(cx).text().trim().to_owned();
        let password = self.quick_password.read(cx).text().to_owned();
        let port = match port_text.parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => {
                self.quick_error = Some(strings.host_field_port.to_owned());
                cx.notify();
                return false;
            }
        };
        if !self.quick_agent && password.is_empty() {
            self.quick_error = Some(strings.auth_missing_password.to_owned());
            cx.notify();
            return false;
        }
        let name = if user.is_empty() {
            address.clone()
        } else {
            format!("{user}@{address}")
        };
        let mut host = match HostConfig::new(name, address, port, user) {
            Ok(host) => host,
            Err(error) => {
                self.quick_error = Some(error.to_string());
                cx.notify();
                return false;
            }
        };
        if self.quick_agent {
            host.set_auth(Some(shellune_core::AuthMethodKind::Agent));
        } else {
            host.set_auth(Some(shellune_core::AuthMethodKind::Password));
        }
        self.stash_focused_panel();
        self.reconnect = None;
        self.form_session = None;
        let mut panel = RemotePanel::new(&host);
        if self.quick_agent {
            panel.select_method(MethodChoice::Agent);
        } else {
            panel.select_method(MethodChoice::Password);
            panel.set_password(password);
        }
        self.pending_ephemeral = Some(host);
        self.remote = Some(panel);
        self.showing_connection_form = true;
        self.quick_error = None;
        self.sync_remote_fields(cx);
        cx.notify();
        true
    }

    /// Writes a quick connection's host, and only then.
    ///
    /// The password is not part of the host record. Cancelling this leaves the
    /// session connected and the file unchanged.
    fn save_quick_host(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(index) = self.unsaved.iter().position(|(session, _)| *session == id) else {
            return;
        };
        let host = self.unsaved[index].1.clone();
        let mut next = self.book.clone();
        if let Err(error) = next.insert(host.clone()) {
            self.hosts.set_message(error.to_string(), true);
            cx.notify();
            return;
        }
        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
                self.unsaved.remove(index);
                self.record_host_connection(host.id(), remote::now_unix(), cx);
                self.hosts
                    .set_message(self.strings().quick_saved.to_owned(), false);
            }
            Err(error) => self.hosts.set_message(error.to_string(), true),
        }
        cx.notify();
    }

    /// Opens the connection form for one saved host.
    ///
    /// An attempt that is already running is parked with its own form. It is not
    /// cancelled, and its session stays. The new form asks only for what this host
    /// still needs.
    pub(crate) fn open_remote(&mut self, host: &HostConfig, cx: &mut Context<Self>) {
        self.stash_focused_panel();
        self.reconnect = None;
        self.form_session = None;
        self.pending_ephemeral = None;
        self.remote = Some(RemotePanel::new(host));
        self.showing_connection_form = true;
        self.sync_remote_fields(cx);
        self.connect_when_ready(host, cx);
        cx.notify();
    }

    /// Fills in what the host remembers and connects when nothing is left to ask.
    ///
    /// A host whose sign-in was chosen in its dialog connects in one click: the saved
    /// password or passphrase comes from the system store, a key file or the agent needs
    /// nothing typed. Anything missing - no choice yet, a secret not remembered, a store
    /// that cannot be read - leaves the form open, saying why when there is a reason.
    fn connect_when_ready(&mut self, host: &HostConfig, cx: &mut Context<Self>) {
        let Some(kind) = host.auth() else {
            return;
        };
        let secret = if host.remembers_secret() {
            match self.secrets.load(host.id()) {
                Ok(secret) => secret,
                Err(error) => {
                    let reason = format!("{}: {error}", self.strings().remote_saved_secret_failed);
                    if let Some(panel) = self.remote.as_mut() {
                        panel.set_form_error(reason);
                    }
                    return;
                }
            }
        } else {
            None
        };
        let Some(panel) = self.remote.as_mut() else {
            return;
        };
        let ready = match kind {
            shellune_core::AuthMethodKind::Agent => true,
            shellune_core::AuthMethodKind::KeyFile => !panel.key_path().trim().is_empty(),
            shellune_core::AuthMethodKind::Password => match &secret {
                Some(secret) => {
                    panel.set_password(secret.expose());
                    true
                }
                None => false,
            },
            shellune_core::AuthMethodKind::EncryptedKeyFile => match &secret {
                Some(secret) if !panel.key_path().trim().is_empty() => {
                    panel.set_passphrase(secret.expose());
                    true
                }
                _ => false,
            },
        };
        if ready {
            self.sync_remote_fields(cx);
            self.connect_remote(cx);
        }
    }

    /// The open connection form, if there is one.
    #[allow(dead_code)]
    #[must_use]
    pub fn remote_state(&self) -> Option<&RemotePanel> {
        self.remote.as_ref()
    }

    /// Why the trust file could not be read, when it could not.
    #[allow(dead_code)]
    #[must_use]
    pub fn trust_error(&self) -> Option<&str> {
        self.trust_error.as_deref()
    }

    /// The accepted identities this shell last read or wrote.
    #[allow(dead_code)]
    #[must_use]
    pub fn trust(&self) -> &HostTrust {
        &self.trust_book
    }

    /// Whether the field one method uses is drawn as bullets.
    #[cfg(test)]
    pub fn remote_field_masked(
        &self,
        method: crate::app::remote::MethodChoice,
        cx: &gpui::App,
    ) -> bool {
        if method.needs_password() {
            self.remote_fields.password.read(cx).is_masked()
        } else {
            self.remote_fields.passphrase.read(cx).is_masked()
        }
    }

    /// Starts the attempt the form describes.
    fn connect_remote(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.remote.as_mut() else {
            return;
        };
        if panel.status().is_busy() {
            return;
        }
        let strings = self.workspace.strings();
        let host = panel.host().clone();
        let auth = match panel.take_auth(strings) {
            Ok(auth) => auth,
            Err(reason) => {
                panel.set_form_error(reason);
                cx.notify();
                return;
            }
        };
        // A reconnected session keeps its record: it is already `Connecting`, and a
        // second record would leave the user with two tabs for one host.
        let id = match self.reconnect.take() {
            Some(id) => {
                // The record is the one that ended; the reconnect is what moves it on
                // to its next process.
                if !self.workspace.session_reconnecting(id) {
                    return;
                }
                id
            }
            None => {
                // The record is created before the transport starts, so a host that
                // never answers is listed as a session that did not open.
                let session = self.workspace.register_remote_session(&host);
                session.id()
            }
        };

        let known = remote::known_host_key(&self.trust_book, &host);
        let size = PtySize::new(100, 30);
        panel.set_status(RemoteStatus::Connecting);
        // The session the open form belongs to, whether it is still connecting or
        // already live: an answer to a trust question and a close both need it.
        let ephemeral = self
            .pending_ephemeral
            .take()
            .is_some_and(|pending| pending.id() == host.id());
        let generation = self
            .workspace
            .session(id)
            .map(Session::generation)
            .unwrap_or(0);
        self.form_session = Some(id);
        self.active = Some(id);
        self.attempts.retain(|attempt| attempt.id != id);
        self.attempts.push(Attempt {
            id,
            generation,
            session: Some(RemoteSession::connect(ConnectRequest::new(
                host.address(),
                host.port().get(),
                host.user(),
                auth,
                size,
                known,
            ))),
            watching: false,
            panel: None,
            ephemeral,
        });
        self.watch_attempt(id, cx);
        cx.notify();
    }

    /// Polls one attempt until it becomes a terminal or fails.
    ///
    /// Each attempt has its own loop. One that ends does not stop the others.
    fn watch_attempt(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(attempt) = self.attempts.iter_mut().find(|attempt| attempt.id == id) else {
            return;
        };
        if attempt.watching || attempt.session.is_none() {
            return;
        }
        attempt.watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REMOTE_POLL_INTERVAL).await;
                match this.update(cx, |view, cx| view.poll_attempt(id, cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Applies what the attempt reported, returning whether to keep watching.
    ///
    /// The events of one poll belong together: the shell usually prints its banner
    /// and prompt immediately after the shell request is accepted, so the connection
    /// notice and the first output arrive in the same batch far more often than not.
    /// Everything drained after the notice is therefore kept and handed to the
    /// terminal once it exists, because the channel has already given those events up
    /// and there is nothing to read them from later.
    fn poll_attempt(&mut self, id: SessionId, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.attempt_index(id) else {
            return false;
        };
        let generation = self.attempts[index].generation;
        if !self
            .workspace
            .session(id)
            .is_some_and(|session| session.accepts_event(generation))
        {
            self.discard_attempt(id);
            return false;
        }
        let events = match self.attempts[index].session.as_mut() {
            Some(session) => session.drain(),
            None => return false,
        };
        let mut pending: Vec<SourceEvent> = Vec::new();
        let mut prompt = None;
        let mut connected = false;
        let mut failed = None;
        for event in events {
            match event {
                RemoteEvent::HostKeyPrompt(asked) => {
                    // Once the terminal is live the identity is settled, so a prompt
                    // arriving behind it is not a question the user can still answer.
                    if !connected {
                        prompt = Some(asked);
                    }
                }
                RemoteEvent::Connected => connected = true,
                RemoteEvent::Failed(error) => {
                    if connected {
                        // The transport failed in the same batch that opened the
                        // terminal: the terminal reports it like any later failure
                        // rather than the panel claiming a session that is gone.
                        pending.push(SourceEvent::Lost(error.to_string()));
                        continue;
                    }
                    failed = Some(error);
                }
                RemoteEvent::Data(_) | RemoteEvent::Exited { .. } => {
                    if !connected {
                        // The transport announces the shell before it reports output,
                        // so output can only belong to a notice that is already in this
                        // batch; before one, there is no terminal to give it to.
                        continue;
                    }
                    if let Some(event) = SourceEvent::from_remote(event) {
                        pending.push(event);
                    }
                }
            }
        }

        if let Some(prompt) = prompt {
            if let Some(panel) = self.panel_for_mut(id) {
                panel.set_status(RemoteStatus::VerifyingHost(prompt));
            }
            cx.notify();
        }
        if let Some(error) = failed {
            self.close_attempt_transport(id);
            self.on_remote_failed(id, &error, cx);
            return false;
        }
        if !connected {
            return true;
        }
        let host = self.panel_for(id).map(|panel| panel.host().clone());
        let ephemeral = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .is_some_and(|attempt| attempt.ephemeral);
        let Some(session) = self.take_transport(id) else {
            return false;
        };
        let Some(host) = host else {
            self.workspace.session_closed(id);
            return false;
        };
        let show = self.form_session == Some(id);
        self.on_remote_connected(id, session, pending, host, ephemeral, show, cx);
        false
    }

    /// Turns a session that reached the remote shell into a terminal of its own.
    fn on_remote_connected(
        &mut self,
        id: SessionId,
        mut session: RemoteSession,
        pending: Vec<SourceEvent>,
        host: HostConfig,
        ephemeral: bool,
        show: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tag) = self.workspace.session(id).map(|session| session.tag()) else {
            return;
        };
        let target = host.target();
        // The browsable side of this connection is taken before the terminal owns the
        // session: the file view then asks on the connection that is already
        // authenticated, and never has to ask for credentials a second time.
        let file_link = session.take_file_link();
        // The transfer side is taken here too, for the same reason: a transfer runs on
        // the connection the shell already authenticated and never asks for credentials
        // of its own.
        let transfer_link = session.take_transfer_link();
        let grid = GridSize::initial();
        let font_size = self.workspace.local_font_size();
        let locale = self.workspace.locale();
        let pane = cx.new(|pane_cx| {
            TerminalPane::start_remote(session, tag, target, grid, font_size, locale, pane_cx)
        });
        // The batch the window drained before the pane existed belongs to this process,
        // and reaches the grid only because the tag matches.
        pane.update(cx, |pane, cx| pane.adopt_events(tag, pending, cx));
        self.workspace.session_verified_and_authenticated(id);
        self.workspace.session_connected(id);
        if show {
            self.form_session = Some(id);
            if let Some(panel) = self.remote.as_mut() {
                panel.set_status(RemoteStatus::Live);
            }
            self.showing_connection_form = false;
        }
        // A quick connection has no host record to mark as recent. Saving it is a
        // separate, explicit step. A saved host is recorded only after it really answered.
        if ephemeral {
            self.unsaved.push((id, host.clone()));
        } else if self.book.host(host.id()).is_some() {
            self.record_host_connection(host.id(), remote::now_unix(), cx);
        }
        if let Some(link) = file_link {
            self.files
                .attach(id, host.name(), host.target(), Box::new(link));
        }
        if let Some(link) = transfer_link {
            self.transfers.attach(id, Box::new(link));
        }
        // The command the host asks for is typed once the shell is there, as the user
        // would type it; the remote side keeps it until its prompt reads it.
        if let Some(command) = host.startup_command() {
            let line = format!("{command}\r");
            pane.update(cx, |pane, cx| pane.type_text(&line, cx));
        }
        self.attach_pane(tag, pane, show, cx);
        // The terminal the user was waiting for is shown at once. A connection that
        // finished in the background only adds its tab, so it does not cover the
        // form or session the user is looking at.
        if show {
            self.activate(id, cx);
        }
    }

    /// Records an attempt that did not open, in both the form and the session list.
    fn on_remote_failed(
        &mut self,
        id: SessionId,
        error: &shellune_ssh::ConnectError,
        cx: &mut Context<Self>,
    ) {
        let failure = RemoteFailure::from_error(error);
        if let Some(panel) = self.panel_for_mut(id) {
            panel.fail(failure);
        }
        self.workspace.session_failed(id, error.session_failure());
        cx.notify();
    }

    /// Answers the question about an unknown server identity.
    ///
    /// Accepting stores the identity first: a connection that was allowed because
    /// the user pressed "accept" must not proceed while the record failed to save,
    /// or the next start would ask again for a decision that was already made. The
    /// answer is then delivered, and if it cannot be — the attempt ended behind the
    /// question — the record is taken back, so a stored identity always belongs to a
    /// connection that really used it.
    pub(crate) fn answer_trust(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(panel) = self.remote.as_ref() else {
            return;
        };
        let Some(prompt) = panel.status().prompt().cloned() else {
            return;
        };
        let Some(id) = self.form_session else {
            // Nothing is waiting for an answer, so no answer is recorded.
            return;
        };
        let strings = self.workspace.strings();
        let now = remote::now_unix();

        if !accept {
            if let Some(session) = self.transport_mut(id) {
                session.answer_host_key(false);
            }
            if let Some(panel) = self.panel_for_mut(id) {
                panel.fail(RemoteFailure::new(
                    strings.trust_rejected_note,
                    strings.remote_close,
                    false,
                ));
            }
            self.workspace
                .session_failed(id, SessionFailure::HostKeyRejected);
            cx.notify();
            return;
        }

        let previous = self.trust_book.clone();
        if let Err(error) = remote::accept_prompted_identity(&mut self.trust_book, &prompt, now) {
            self.trust_book = previous;
            self.refuse_with(
                id,
                RemoteFailure::new(error.to_string(), strings.remote_close, false),
                cx,
            );
            return;
        }
        if let Err(error) = self.trust.save(&self.trust_book) {
            self.trust_book = previous;
            self.refuse_with(
                id,
                RemoteFailure::new(error.to_string(), strings.remote_close, false),
                cx,
            );
            return;
        }
        // The record is only worth having if the connection actually uses it: an
        // attempt that already ended (a timeout behind the question, a form that was
        // closed, a server that hung up) cannot be told to accept anything, so the
        // record is taken back rather than left claiming a decision that nothing used.
        let delivered = self
            .transport_mut(id)
            .is_some_and(|session| session.answer_host_key(true));
        if !delivered {
            // The file gets the book that was there before this answer, so nothing
            // the retracted acceptance added survives a restart.
            self.trust_book = previous;
            let _ = self.trust.save(&self.trust_book);
            if let Some(mut session) = self.take_transport(id) {
                session.close();
            }
            if let Some(panel) = self.panel_for_mut(id) {
                panel.fail(RemoteFailure::new(
                    strings.trust_undelivered_note,
                    strings.remote_close,
                    true,
                ));
            }
            // The attempt ended before the decision could reach it, which is not a
            // refusal of the identity: the record says the session never opened.
            self.workspace.session_failed(
                id,
                SessionFailure::ConnectionFailed {
                    reason: strings.trust_undelivered_note.to_owned(),
                },
            );
            cx.notify();
            return;
        }
        if let Some(panel) = self.remote.as_mut() {
            panel.set_status(RemoteStatus::Connecting);
        }
        cx.notify();
    }

    /// Ends an attempt because the identity could not be recorded.
    fn refuse_with(&mut self, id: SessionId, failure: RemoteFailure, cx: &mut Context<Self>) {
        if let Some(session) = self.transport_mut(id) {
            session.answer_host_key(false);
        }
        if let Some(mut session) = self.take_transport(id) {
            session.close();
        }
        if let Some(panel) = self.panel_for_mut(id) {
            panel.fail(failure);
        }
        self.workspace
            .session_failed(id, SessionFailure::HostKeyRejected);
        cx.notify();
    }

    /// Shows the identity question, asked by an attempt that is really waiting.
    ///
    /// A live server cannot be made to present an unknown identity on demand inside
    /// a test, so the attempt is a scripted one: it reports the same prompt event the
    /// transport emits, and its answer channel is read, so a test can prove that a
    /// decision which stores a record was actually delivered.
    #[cfg(test)]
    pub(crate) fn show_trust_prompt(
        &mut self,
        prompt: shellune_ssh::HostKeyPrompt,
        cx: &mut Context<Self>,
    ) {
        if let Some(host) = self.remote.as_ref().map(|panel| panel.host().clone()) {
            let session = self.workspace.register_remote_session(&host);
            let id = session.id();
            self.form_session = Some(id);
            self.workspace.session_verified_and_authenticated(id);
            let scripted = shellune_ssh::ScriptedSession::new();
            scripted.push(RemoteEvent::HostKeyPrompt(prompt));
            self.install_scripted_session(id, scripted, cx);
        }
        // The prompt reaches the form the same way a real one does.
        if let Some(id) = self.form_session {
            self.poll_attempt(id, cx);
        }
    }

    /// Puts a session whose events a test supplies in place of a connecting attempt.
    ///
    /// Only the attempt with `id` is replaced. Any other attempt keeps its transport.
    #[cfg(test)]
    pub(crate) fn install_scripted_session(
        &mut self,
        id: SessionId,
        mut scripted: shellune_ssh::ScriptedSession,
        cx: &mut Context<Self>,
    ) {
        let generation = self
            .workspace
            .session(id)
            .map(Session::generation)
            .unwrap_or(0);
        let ephemeral = if self.pending_ephemeral.as_ref().is_some_and(|host| {
            self.remote
                .as_ref()
                .is_some_and(|panel| panel.host().id() == host.id())
        }) {
            self.pending_ephemeral = None;
            true
        } else {
            self.attempts
                .iter()
                .find(|attempt| attempt.id == id)
                .is_some_and(|attempt| attempt.ephemeral)
        };
        self.attempts.retain(|attempt| attempt.id != id);
        self.attempts.push(Attempt {
            id,
            generation,
            session: Some(scripted.take_session()),
            watching: false,
            panel: None,
            ephemeral,
        });
        self.scripted.retain(|(session, _)| *session != id);
        self.scripted.push((id, scripted));
        cx.notify();
    }

    /// Starts an attempt that reports the given events instead of dialling a host.
    ///
    /// Everything else is the real path: the session record is created where a real
    /// attempt creates it, the form says it is connecting, and the events are applied
    /// by the same poll the window uses.
    #[cfg(test)]
    pub(crate) fn script_remote_attempt(
        &mut self,
        events: Vec<RemoteEvent>,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.remote.as_ref().map(|panel| panel.host().clone()) else {
            return;
        };
        let id = match self.reconnect.take() {
            Some(id) => {
                if !self.workspace.session_reconnecting(id) {
                    return;
                }
                id
            }
            None => self.workspace.register_remote_session(&host).id(),
        };
        self.form_session = Some(id);
        self.active = Some(id);
        self.showing_connection_form = true;
        let scripted = shellune_ssh::ScriptedSession::new();
        for event in events {
            scripted.push(event);
        }
        // The script is moved into the view, which reads the answers it received.
        self.install_scripted_session(id, scripted, cx);
        if let Some(panel) = self.remote.as_mut() {
            panel.set_status(RemoteStatus::Connecting);
        }
        cx.notify();
    }

    /// Ends the attempt behind an open question, the way a connection thread that
    /// stopped would: the question stays on screen, but nothing can answer it.
    #[cfg(test)]
    pub(crate) fn end_scripted_attempt(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.form_session {
            let _ = self.take_transport(id);
            self.scripted.retain(|(session, _)| *session != id);
        }
        cx.notify();
    }

    /// The answers the focused scripted attempt received, in the order they were given.
    #[cfg(test)]
    pub(crate) fn scripted_answers(&self) -> Vec<shellune_ssh::TrustAnswer> {
        let focused = self.form_session;
        self.scripted
            .iter()
            .rev()
            .find(|(id, _)| focused.is_none_or(|focused| *id == focused))
            .map(|(_, scripted)| scripted.answers())
            .unwrap_or_default()
    }

    /// How many connection attempts still have a transport.
    #[cfg(test)]
    pub(crate) fn pending_attempt_count(&self) -> usize {
        self.attempts
            .iter()
            .filter(|attempt| attempt.session.is_some())
            .count()
    }

    /// Applies one round of remote events for every attempt, the way each poll loop does.
    ///
    /// The loop itself waits on the window timer, which a test cannot advance, so a
    /// test drives the same call directly. The result is whether any attempt is still
    /// waiting.
    #[cfg(test)]
    pub(crate) fn pump_remote_once(&mut self, cx: &mut Context<Self>) -> bool {
        let ids: Vec<SessionId> = self.attempts.iter().map(|attempt| attempt.id).collect();
        let mut waiting = false;
        for id in ids {
            if self.poll_attempt(id, cx) {
                waiting = true;
            }
        }
        waiting
    }

    /// Runs a failed attempt through the same path the transport uses.
    ///
    /// A test cannot open a real SSH connection to a host that refuses it inside the
    /// window, so the failure is delivered here exactly as the poll loop delivers it:
    /// through the session record and the form.
    #[cfg(test)]
    pub(crate) fn fail_remote_for_test(
        &mut self,
        error: shellune_ssh::ConnectError,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.remote.as_ref().map(|panel| panel.host().clone()) else {
            return;
        };
        let id = match self.reconnect.take() {
            Some(id) => id,
            None => self.workspace.register_remote_session(&host).id(),
        };
        self.form_session = Some(id);
        self.on_remote_failed(id, &error, cx);
    }

    /// Tries again with whatever the form holds now.
    fn retry_remote(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.remote.as_mut() {
            panel.set_status(RemoteStatus::Idle);
        }
        self.connect_remote(cx);
    }

    /// Closes the connection form on screen and the attempt it belongs to.
    ///
    /// Other attempts keep running. A reconnect the user abandoned leaves its
    /// session as it was: disconnected, with the reason it ended.
    fn close_remote(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.form_session {
            self.close_session(id, cx);
        } else {
            if let Some(panel) = self.remote.as_mut() {
                panel.clear_secrets();
            }
            self.remote = None;
            self.showing_connection_form = false;
            cx.notify();
        }
        self.reconnect = None;
        self.pending_ephemeral = None;
        cx.notify();
    }

    /// Records that a host was really reached, and stores the record.
    ///
    /// The remote-connection step calls this after a session reached
    /// `Connected`. Nothing in host management itself calls it, which is why saving
    /// a host never makes it appear in the recent view.
    ///
    /// Returns whether the record reached the file.
    #[allow(dead_code)]
    pub fn record_host_connection(
        &mut self,
        id: HostId,
        at_unix: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut next = self.book.clone();
        if let Err(error) = next.record_connection(id, at_unix) {
            self.hosts.set_message(error.to_string(), true);
            cx.notify();
            return false;
        }
        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                cx.notify();
                true
            }
            Err(error) => {
                self.hosts.set_message(error.to_string(), true);
                cx.notify();
                false
            }
        }
    }

    /// Starts the shell named in the free-text field.
    ///
    /// The text is taken as written: a wrong path becomes a real launch failure,
    /// which is the only honest answer to a program that does not exist.
    fn start_custom_shell(
        &mut self,
        program: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let program = program.trim().to_owned();
        if program.is_empty() {
            return false;
        }
        let shell = LocalShell::new(program.clone(), program);
        self.start_shell(shell, Some(window), cx);
        // Only a start that really produced a terminal may take the field away. Asking
        // whether *some* pane is on screen is not the same question: a shell that
        // failed to launch leaves the previous terminal in place and selected, so the
        // old check closed and emptied the field over the path it had just rejected,
        // and the user could not correct it. A launch failure is reported through
        // `local_error`, so that is what decides here.
        let started = self.local_error.is_none() && self.active_pane().is_some();
        if started {
            // The shell that started owns the keyboard, so what the user types next
            // goes to it. The field closes and, because this returns true, empties:
            // a second Return over the same path would start the shell again
            // instead of reaching it.
            self.custom_shell_open = false;
            self.shell_menu_open = false;
            cx.notify();
        }
        started
    }

    /// Starts whichever shell the picker currently points at.
    fn start_from_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = if self.custom_shell_open {
            self.shell_path.read(cx).text().trim().to_owned()
        } else {
            String::new()
        };
        let shell = if typed.is_empty() {
            match self.workspace.selected_shell() {
                Some(shell) => shell.clone(),
                None => {
                    let reason = self.strings().shell_none_found.to_owned();
                    self.workspace
                        .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
                    cx.notify();
                    return;
                }
            }
        } else {
            LocalShell::new(typed.clone(), typed)
        };
        self.start_shell(shell, Some(window), cx);
    }

    /// Starts a shell and, when it really started, records the session.
    ///
    /// A new shell is a new tab: the sessions that are already open keep their
    /// processes and their screens, which is the difference between opening a second
    /// terminal and restarting the first one. A launch failure keeps no pane and no
    /// session record, so the destination cannot show a terminal that has no process
    /// behind it.
    fn start_shell(
        &mut self,
        shell: LocalShell,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.local_error = None;
        let grid = GridSize::initial();
        let font_size = self.workspace.local_font_size();
        let locale = self.workspace.locale();
        let session = self.workspace.register_local_session(&shell);
        let id = session.id();
        let tag = session.tag();
        let pane =
            cx.new(|pane_cx| TerminalPane::start(shell, tag, grid, font_size, locale, pane_cx));
        let state = pane.read(cx).state().clone();
        if !state.accepts_input() {
            let reason = state
                .failure()
                .map_or_else(|| state.label(locale), |failure| failure.to_string());
            // A process that never ran is not a session: the record is dropped and the
            // reason is reported where the picked shell is.
            self.workspace.session_forgotten(id);
            self.local_error = Some(reason.clone());
            self.workspace
                .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
            cx.notify();
            return;
        }

        self.workspace.session_connected(id);
        self.workspace.clear_session_failure();
        // The size the pane actually uses is the one the next session starts with.
        self.workspace
            .set_local_font_size(pane.read(cx).font_size());
        if let Some(window) = window {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
        }
        self.push_pane(tag, pane, cx);
    }

    /// Starts the process of a session that is already recorded, and shows it.
    ///
    /// The reconnect path uses this: the record, its title and its place in the list
    /// are already there, so only the process and the terminal are new.
    fn start_local_pane(&mut self, id: SessionId, shell: LocalShell, cx: &mut Context<Self>) {
        let program = shell.program().to_owned();
        let grid = GridSize::initial();
        let font_size = self.workspace.local_font_size();
        let locale = self.workspace.locale();
        let Some(tag) = self.workspace.session(id).map(|session| session.tag()) else {
            return;
        };
        let pane =
            cx.new(|pane_cx| TerminalPane::start(shell, tag, grid, font_size, locale, pane_cx));
        let state = pane.read(cx).state().clone();
        if !state.accepts_input() {
            let reason = state
                .failure()
                .map_or_else(|| state.label(locale), |failure| failure.to_string());
            self.local_error = Some(reason.clone());
            self.workspace.session_failed(
                id,
                SessionFailure::LocalShellLaunchFailed {
                    shell: program,
                    reason,
                },
            );
            cx.notify();
            return;
        }
        self.workspace.session_connected(id);
        self.workspace.clear_session_failure();
        self.workspace
            .set_local_font_size(pane.read(cx).font_size());
        self.push_pane(tag, pane, cx);
    }

    /// Mirrors what one pane reports into the session it belongs to.
    ///
    /// The pane owns the truth about its process; the list only repeats it, so a shell
    /// that exited cannot stay listed as running. The session is read from the pane's
    /// own entry, so a pane can never update a session other than its own.
    fn sync_pane(&mut self, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
        let tag = pane.read(cx).tag();
        let Some(id) = self
            .panes
            .iter()
            .find(|open| open.pane.entity_id() == pane.entity_id())
            .map(|open| open.tag.session)
        else {
            // The pane was closed: what it reports no longer describes a session the
            // shell shows.
            return;
        };
        if !self.workspace.session_accepts_event(tag) {
            return;
        }
        let state = pane.read(cx).state().clone();
        match state {
            PaneState::Live => {
                self.workspace.session_connected(id);
            }
            PaneState::Exited { report } => {
                // A shell that exits on its own is still over. Reporting the status it
                // exited with is the difference between "the session ended" and a
                // session list that keeps claiming a process that is gone.
                let failure = report
                    .session_failure()
                    .unwrap_or(shellune_core::SessionFailure::ProcessExited { code: Some(0) });
                self.workspace.session_failed(id, failure);
                // A transfer that was running on this connection has lost it: it is
                // reported as failed, never as completed and never as something the user
                // cancelled, so a partial file is never presented as a finished one.
                self.transfers
                    .session_lost(id, shellune_core::TransferFailure::ConnectionLost);
                self.end_files(id);
            }
            PaneState::Failed(failure) => {
                self.workspace.session_failed(id, failure);
                self.transfers
                    .session_lost(id, shellune_core::TransferFailure::ConnectionLost);
                self.end_files(id);
            }
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Remote connections: the panel (R4, R6)
    // -----------------------------------------------------------------------

    /// The quick-connection form. It never writes a host by itself.
    fn quick_panel(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut card = components::card(metrics)
            .w_full()
            .max_w(metrics.scale(480.0))
            .debug_selector(|| "quick.panel".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(metrics.title_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(strings.quick_connect_title),
                    )
                    .child(
                        components::icon_only_button(Icon::Close, metrics)
                            .id("quick.close")
                            .debug_selector(|| "quick.close".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.quick_open = false;
                                this.quick_error = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(strings.quick_connect_hint),
            )
            .child(self.remote_field(
                SharedString::from("quick.address"),
                strings.host_field_address,
                self.quick_address.clone(),
            ))
            .child(
                div()
                    .flex()
                    .gap(metrics.scale(spacing::SNUG))
                    .child(div().w(metrics.scale(120.0)).child(self.remote_field(
                        SharedString::from("quick.port"),
                        strings.host_field_port,
                        self.quick_port.clone(),
                    )))
                    .child(div().flex_1().child(self.remote_field(
                        SharedString::from("quick.user"),
                        strings.host_field_user,
                        self.quick_user.clone(),
                    ))),
            );
        if !self.quick_agent {
            card = card.child(self.remote_field(
                SharedString::from("quick.password"),
                strings.auth_password,
                self.quick_password.clone(),
            ));
        }
        card = card.child(
            self.text_button(
                SharedString::from("quick.agent"),
                strings.auth_agent,
                if self.quick_agent {
                    Variant::Primary
                } else {
                    Variant::Secondary
                },
                cx,
                |view, cx| {
                    view.quick_agent = !view.quick_agent;
                    cx.notify();
                },
            )
            .debug_selector(|| "quick.agent".to_owned()),
        );
        if let Some(reason) = &self.quick_error {
            card = card.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Danger.text())
                    .debug_selector(|| "quick.error".to_owned())
                    .child(SharedString::from(reason.clone())),
            );
        }
        card.child(
            div().flex().justify_end().child(
                self.icon_text_button(
                    SharedString::from("quick.connect"),
                    Icon::Plug,
                    strings.remote_connect,
                    Variant::Primary,
                    cx,
                    |view, cx| view.start_quick_connect(cx),
                )
                .debug_selector(|| "quick.connect".to_owned()),
            ),
        )
    }

    /// Attempts that are not the form on screen, each with its own cancel.
    fn other_attempts(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let others: Vec<(usize, SessionId)> = self
            .attempts
            .iter()
            .enumerate()
            .filter(|(_, attempt)| self.form_session != Some(attempt.id))
            .filter(|(_, attempt)| {
                attempt.session.is_some()
                    || attempt
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.status().failure().is_some())
            })
            .map(|(index, attempt)| (index, attempt.id))
            .collect();
        if others.is_empty() {
            return div();
        }
        let mut column = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "attempt.others".to_owned())
            .child(components::section_label(strings.attempt_others, metrics));
        for (index, id) in others {
            let title = self
                .panel_for(id)
                .map(|panel| panel.host().name().to_owned())
                .or_else(|| {
                    self.workspace
                        .session(id)
                        .map(|session| session.title().to_owned())
                })
                .unwrap_or_default();
            let status = self
                .workspace
                .session(id)
                .map(|session| copy::session_state_label(session.state(), self.workspace.locale()))
                .unwrap_or_default();
            column = column.child(
                components::row(metrics)
                    .id(("attempt.row", index))
                    .debug_selector({
                        let selector = format!("attempt.row.{index}");
                        move || selector
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.focus_attempt(id, cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(components::stacked(title, status, metrics)),
                    )
                    .child({
                        let selector = format!("attempt.cancel.{index}");
                        components::compact_button(strings.attempt_cancel, Variant::Danger, metrics)
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector)
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                // The row opens the attempt; this button only ends it.
                                cx.stop_propagation();
                                this.close_session(id, cx);
                            }))
                    }),
            );
        }
        column
    }

    /// The connection form, the trust question and the remote terminal.
    ///
    /// Everything on it is drawn from the state the transport reported: a host that
    /// has not answered shows the form, an unknown identity shows the fingerprint and
    /// two answers, and a live session shows the same terminal pane a local shell
    /// uses. Nothing here can show a terminal for an attempt that failed.
    fn remote_panel(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let Some(panel) = self.remote.as_ref() else {
            return div();
        };
        let status = panel.status();
        let host = panel.host();
        let (status_label, tone) = match status {
            RemoteStatus::Idle => (strings.remote_idle, Tone::Neutral),
            RemoteStatus::Connecting => (strings.remote_connecting, Tone::Warning),
            RemoteStatus::VerifyingHost(_) => (strings.remote_verifying, Tone::Warning),
            RemoteStatus::Live => (strings.remote_live, Tone::Positive),
            RemoteStatus::Failed(_) => (strings.remote_failed, Tone::Danger),
        };

        let mut column = components::card(metrics)
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .debug_selector(|| "remote.panel".to_owned());

        column = column.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::TIGHT))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .min_w(px(0.0))
                        .child(host_avatar(host.name(), false, metrics))
                        .child(components::stacked(
                            host.name().to_owned(),
                            host.target(),
                            metrics,
                        ))
                        .child(components::chip(status_label, tone, metrics)),
                )
                .child(self.icon_text_button(
                    SharedString::from("remote.close"),
                    Icon::Close,
                    strings.remote_close,
                    Variant::Ghost,
                    cx,
                    |view, cx| view.close_remote(cx),
                )),
        );

        if let Some(prompt) = status.prompt() {
            let algorithm = prompt.algorithm().to_owned();
            let fingerprint = prompt.fingerprint().to_owned();
            column = column.child(
                components::banner(Tone::Warning, metrics)
                    .gap(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "remote.trust".to_owned())
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(strings.trust_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(strings.trust_body),
                    )
                    .child(self.trust_line(
                        strings.trust_algorithm,
                        "remote.trust.algorithm",
                        algorithm,
                    ))
                    .child(self.trust_line(
                        strings.trust_fingerprint,
                        "remote.trust.fingerprint",
                        fingerprint,
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .child(self.text_button(
                                SharedString::from("remote.trust.accept"),
                                strings.trust_accept,
                                Variant::Primary,
                                cx,
                                |view, cx| view.answer_trust(true, cx),
                            ))
                            .child(self.text_button(
                                SharedString::from("remote.trust.reject"),
                                strings.trust_reject,
                                Variant::Danger,
                                cx,
                                |view, cx| view.answer_trust(false, cx),
                            )),
                    ),
            );
        }

        if let Some(failure) = status.failure() {
            let reason = failure.reason().to_owned();
            let next_step =
                copy::next_step_text(failure.next_step(), self.workspace.locale()).to_owned();
            let retryable = failure.retryable();
            column = column.child(
                components::banner(Tone::Danger, metrics)
                    .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                    .py(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "remote.failure".to_owned())
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Tone::Danger.text())
                            .child(strings.remote_failure_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "remote.failure.reason".to_owned())
                            .child(SharedString::from(reason)),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "remote.failure.next".to_owned())
                            .child(SharedString::from(format!(
                                "{}: {next_step}",
                                strings.next_step
                            ))),
                    )
                    .when(retryable, |card| {
                        card.child(div().flex().pt(metrics.scale(spacing::HAIRLINE)).child(
                            self.text_button(
                                SharedString::from("remote.retry"),
                                strings.recovery_retry,
                                Variant::Primary,
                                cx,
                                |view, cx| view.retry_remote(cx),
                            ),
                        ))
                    }),
            );
        }

        if let Some(pane) = self.form_session.and_then(|id| self.pane_for(id))
            && matches!(status, RemoteStatus::Live)
        {
            // A terminal needs rows: inside a scrolling list it would otherwise be
            // squeezed to the height of its own border. The sessions destination
            // gives it the whole panel; here it keeps a usable fixed height.
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .h(px(REMOTE_TERMINAL_HEIGHT))
                    .rounded(px(radius::LARGE))
                    .bg(theme::swatch(palette::CANVAS))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .child(pane.clone()),
            );
        }

        if !matches!(status, RemoteStatus::Live) && !status.is_asking() {
            column = column.child(self.remote_form(cx));
        }

        column
    }

    /// One labelled value under the trust question.
    fn trust_line(&self, label: &'static str, selector: &'static str, value: String) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .min_w(px(0.0))
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(label),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(move || selector.to_owned())
                    .child(SharedString::from(value)),
            )
    }

    /// The method chooser and the fields the chosen method needs.
    fn remote_form(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let Some(panel) = self.remote.as_ref() else {
            return div();
        };
        let method = panel.method();
        let busy = panel.status().is_busy();

        let mut form = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG - 2.0))
            .debug_selector(|| "remote.form".to_owned());

        let mut methods = components::segmented(metrics);
        for choice in MethodChoice::ALL {
            let selected = choice == method;
            methods = methods.child(
                components::segment(choice.label(strings), selected, metrics)
                    .id(("remote.method", choice as usize))
                    .debug_selector(move || choice.debug_selector().to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        if let Some(panel) = this.remote.as_mut() {
                            panel.select_method(choice);
                        }
                        cx.notify();
                    })),
            );
        }
        form = form.child(div().flex().child(methods));
        form = form.child(caption_line(method.hint(strings).to_owned(), metrics));

        if method.needs_password() {
            form = form.child(self.remote_field(
                SharedString::from("remote.password"),
                strings.auth_password,
                self.remote_fields.password.clone(),
            ));
        }
        if method.needs_key_path() {
            form = form.child(self.remote_field(
                SharedString::from("remote.key"),
                strings.auth_key,
                self.remote_fields.key_path.clone(),
            ));
        }
        if method.needs_passphrase() {
            form = form.child(self.remote_field(
                SharedString::from("remote.passphrase"),
                strings.auth_key_passphrase,
                self.remote_fields.passphrase.clone(),
            ));
        }
        if !method.needs_key_path() && !method.needs_password() {
            form = form.child(caption_line(
                format!(
                    "{}: {}",
                    strings.auth_agent,
                    remote::default_agent().label()
                ),
                metrics,
            ));
        }

        if let Some(reason) = panel.form_error() {
            form = form.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Danger.text())
                    .debug_selector(|| "remote.form.error".to_owned())
                    .child(SharedString::from(reason.to_owned())),
            );
        }

        if busy {
            form = form.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Warning.text())
                    .debug_selector(|| "remote.busy".to_owned())
                    .child(strings.remote_connecting),
            );
        } else {
            form = form.child(
                div().flex().justify_end().child(
                    self.icon_text_button(
                        SharedString::from("remote.connect"),
                        Icon::Plug,
                        strings.remote_connect,
                        Variant::Primary,
                        cx,
                        |view, cx| view.connect_remote(cx),
                    )
                    .h(metrics.control_height())
                    .px(metrics.scale(spacing::BASE)),
                ),
            );
        }
        form
    }

    /// One labelled credential field.
    fn remote_field(
        &self,
        selector: SharedString,
        label: &'static str,
        field: Entity<TextField>,
    ) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w(px(0.0))
                    .debug_selector(move || selector.to_string())
                    .child(field),
            )
    }

    // -----------------------------------------------------------------------
    // Hosts destination (R3)
    // -----------------------------------------------------------------------

    /// The hosts destination: where the records live, the search and view switch,
    /// the form, the deletion question and the list the current view selects.
    ///
    /// A store that cannot be read renders as a state card with its reason and a
    /// retry, because "no hosts" and "the file is broken" are different answers.
    fn hosts_destination(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let state = self.workspace.state(Destination::Hosts);

        match state {
            SectionState::Loading => {
                return self.section_card(Destination::Hosts, state);
            }
            SectionState::Failed => {
                return self
                    .section_card(Destination::Hosts, state)
                    .child(self.text_button(
                        SharedString::from("hosts.retry"),
                        strings.hosts_retry,
                        Variant::Primary,
                        cx,
                        |view, cx| view.reload_hosts(cx),
                    ));
            }
            SectionState::Empty | SectionState::Populated => {}
        }

        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(metrics.scale(spacing::SNUG));
        // A connection in progress belongs at the top of the destination that
        // started it, beside the host it is for. Attempts that are not the form on
        // screen stay listed after that form is closed, so each one can still be
        // cancelled on its own.
        if self.remote.is_some() {
            column = column.child(self.remote_panel(cx));
        }
        column = column.child(self.other_attempts(cx));
        // While the host dialog is open, its own refusals are shown inside it.
        if let Some(message) = self
            .hosts
            .message()
            .filter(|message| !(message.failed() && self.host_fields.is_some()))
        {
            column = column.child(
                components::banner(
                    if message.failed() {
                        Tone::Danger
                    } else {
                        Tone::Positive
                    },
                    metrics,
                )
                .debug_selector(|| "hosts.message".to_owned())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(SharedString::from(message.text().to_owned())),
            );
        }

        // An empty store with no search, no open form and no open import explains
        // itself; anything else shows the tools and the list.
        if self.book.hosts().is_empty()
            && self.hosts.search().trim().is_empty()
            && !self.hosts.is_editing()
            && !self.import.is_open()
        {
            return column
                .child(
                    self.section_card(Destination::Hosts, state)
                        .child(self.hosts_import_entry(cx)),
                )
                .child(self.hosts_storage_line());
        }

        column = column.child(self.hosts_toolbar(cx));
        if self.import.is_open() {
            column = column.child(self.import_panel(cx));
        }
        if let Some(id) = self.hosts.pending_delete() {
            column = column.child(self.host_delete_bar(id, cx));
        }
        column
            .child(self.hosts_list(cx))
            .child(self.hosts_storage_line())
    }

    /// Where the records are stored, said plainly.
    fn hosts_storage_line(&self) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
            .min_w(px(0.0))
            .pt(metrics.scale(spacing::TIGHT))
            .id("hosts.storage")
            .debug_selector(|| "hosts.storage".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.hosts_storage_label),
            )
            .child(
                div()
                    .truncate()
                    .font_family(terminal::monospace_family())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(home_relative(
                        &self.store.path().display().to_string(),
                    ))),
            )
            .tooltip(components::tooltip(SharedString::from(
                self.store.path().display().to_string(),
            )))
    }

    /// The search field and the three list views.
    fn hosts_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut views = components::segmented(metrics);
        for view in HostView::ALL {
            let selected = self.hosts.view() == view;
            views = views.child(
                components::segment(view.label(strings), selected, metrics)
                    .id(view.debug_selector())
                    .debug_selector(move || view.debug_selector().to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.hosts.select_view(view);
                        cx.notify();
                    })),
            );
        }

        // Importing sits beside the views because it is how a list gets its first
        // records; the panel it opens states its limits before anything is saved.
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "hosts.toolbar".to_owned())
            .child(
                div()
                    .flex_1()
                    .min_w(metrics.scale(180.0))
                    .debug_selector(|| "hosts.search".to_owned())
                    .child(self.hosts_search.clone()),
            )
            .child(views)
            .child(self.import_button(cx))
    }

    /// The button that opens the import panel.
    fn import_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.icon_text_button(
            SharedString::from("hosts.import"),
            Icon::Import,
            self.strings().import_open,
            Variant::Secondary,
            cx,
            |view, cx| view.open_import(cx),
        )
        .h(self.metrics().control_height())
    }

    /// The import entry an empty list offers, since a first run has no hosts to show
    /// beside it.
    fn hosts_import_entry(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .child(self.import_button(cx))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .text_center()
                    .child(strings.import_read_only),
            )
    }

    /// The import preview: the file it read, its limits and every host it describes.
    ///
    /// Nothing here is decided by the drawing code: the outcome, the notes and the
    /// count of hosts that can be imported all come from the importer, which is what
    /// keeps a partial import from looking like a complete one.
    fn import_panel(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();

        let path_field = match &self.import_path {
            Some(field) => div()
                .flex_1()
                .min_w(px(0.0))
                .debug_selector(|| "hosts.import.path".to_owned())
                .child(field.clone()),
            None => div().flex_1(),
        };

        let mut card = components::card(metrics)
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .debug_selector(|| "hosts.import.panel".to_owned())
            .child(components::card_title(
                strings.import_title,
                Some(SharedString::from(strings.import_read_only)),
                metrics,
            ))
            .child(
                components::field_row(strings.import_path_label, metrics)
                    .child(path_field)
                    .child(self.text_button(
                        SharedString::from("hosts.import.preview"),
                        strings.import_preview,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.preview_import(cx),
                    )),
            );

        if let Some(error) = self.import.error() {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(1.0))
                    .debug_selector(|| "hosts.import.error".to_owned())
                    .child(
                        div()
                            .text_size(metrics.label_size())
                            .text_color(Tone::Danger.text())
                            .child(strings.import_error_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(error.to_owned())),
                    ),
            );
            if !self.import.keeps_preview() {
                return card;
            }
        }

        let Some(preview) = self.import.preview() else {
            return card;
        };

        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .debug_selector(|| "hosts.import.source".to_owned())
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(strings.import_source_label),
                )
                .child(
                    div()
                        .truncate()
                        .min_w(px(0.0))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(preview.source().display().to_string())),
                ),
        );

        card = card.child(self.import_limits(preview));

        for (index, host) in preview.hosts().iter().enumerate() {
            card = card.child(self.import_row(index, host));
        }

        let mut actions = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .mt(metrics.scale(spacing::HAIRLINE));

        let offered = preview.importable_count();
        if offered > 0 {
            actions = actions.child(self.text_button(
                SharedString::from("hosts.import.apply"),
                SharedString::from(format!("{} · {offered}", strings.import_apply)),
                Variant::Primary,
                cx,
                |view, cx| view.apply_import(cx),
            ));
        } else {
            actions = actions.child(
                div()
                    .debug_selector(|| "hosts.import.nothing".to_owned())
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.import_nothing),
            );
        }

        card.child(actions.child(self.text_button(
            SharedString::from("hosts.import.close"),
            strings.host_cancel,
            Variant::Secondary,
            cx,
            |view, cx| view.close_import(cx),
        )))
    }

    /// What the file could not contribute, stated before anything is applied.
    fn import_limits(&self, preview: &ImportPreview) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let limits = preview.limits();
        let ready = preview
            .hosts()
            .iter()
            .filter(|host| host.outcome() == ImportOutcome::Ready && host.is_importable())
            .count();

        let mut block = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(1.0))
            .w_full()
            .debug_selector(|| "hosts.import.limits".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.import_limits_title),
            )
            .child(caption_line(
                format!(
                    "{} {} · {} {} · {} {}",
                    strings.import_outcome_ready,
                    ready,
                    strings.import_outcome_review,
                    preview.review_count(),
                    strings.import_outcome_blocked,
                    preview.blocked_count()
                ),
                metrics,
            ));

        if !limits.ignored().is_empty() {
            let keywords: Vec<String> = limits
                .ignored()
                .iter()
                .map(|note| format!("{} (line {})", note.keyword(), note.line()))
                .collect();
            block = block.child(caption_line(
                format!("{} · {}", strings.import_ignored_label, keywords.join(", ")),
                metrics,
            ));
        }
        if !limits.include_lines().is_empty() {
            block = block.child(caption_line(
                format!(
                    "{} · {}",
                    strings.import_include_label,
                    line_list(limits.include_lines())
                ),
                metrics,
            ));
        }
        if !limits.match_lines().is_empty() {
            block = block.child(caption_line(
                format!(
                    "{} · {}",
                    strings.import_match_label,
                    line_list(limits.match_lines())
                ),
                metrics,
            ));
        }
        if !limits.unmatched_proxy().is_empty() {
            let patterns: Vec<String> = limits
                .unmatched_proxy()
                .iter()
                .map(|note| format!("{} (line {})", note.keyword(), note.line()))
                .collect();
            block = block.child(caption_line(
                format!(
                    "{} · {}",
                    strings.import_unmatched_proxy_label,
                    patterns.join(", ")
                ),
                metrics,
            ));
        }
        if !preview.is_partial() {
            block = block.child(caption_line(strings.import_limits_none.to_owned(), metrics));
        }
        block
    }

    /// One host the file describes, with the outcome and every reason behind it.
    fn import_row(&self, index: usize, host: &ImportedHost) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let (tone, label) = match host.outcome() {
            ImportOutcome::Ready => (Tone::Positive, strings.import_outcome_ready),
            ImportOutcome::NeedsReview => (Tone::Warning, strings.import_outcome_review),
            ImportOutcome::Blocked => (Tone::Danger, strings.import_outcome_blocked),
        };
        let chip = if host.is_importable() {
            SharedString::from(label)
        } else {
            SharedString::from(format!("{label} · {}", strings.import_not_offered))
        };

        let mut row = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(1.0))
            .w_full()
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(move || format!("hosts.import.row.{index}"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .min_w(px(0.0))
                            .child(components::status_dot(tone, metrics))
                            .child(components::stacked(
                                host.alias().to_owned(),
                                host.summary(),
                                metrics,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .child(components::chip(chip, tone, metrics)),
                    ),
            );

        for note in host.notes() {
            row = row.child(caption_line(note.reason().to_owned(), metrics));
        }
        row
    }

    /// The question shown before a host is deleted.
    fn host_delete_bar(&self, id: HostId, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let target = self.book.host(id).map_or_else(String::new, |host| {
            format!("{} ({})", host.name(), host.target())
        });

        components::banner(Tone::Danger, metrics)
            .flex_row()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::SNUG))
            .debug_selector(|| "hosts.confirm".to_owned())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(1.0))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(format!(
                                "{} {target}",
                                strings.host_delete_question
                            ))),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.host_delete_note),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.text_button(
                        SharedString::from("hosts.confirm.delete"),
                        strings.host_delete_confirm,
                        Variant::Danger,
                        cx,
                        |view, cx| view.confirm_delete_host(cx),
                    ))
                    .child(self.text_button(
                        SharedString::from("hosts.confirm.cancel"),
                        strings.host_cancel,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.cancel_delete_host(cx),
                    )),
            )
    }

    /// The saved hosts, in the shape the current view selects.
    fn hosts_list(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        // The number of rows the current view lists is part of the selector, so a
        // test can tell "the search narrowed the list" from "the list did not
        // change" without reading text.
        let entries = match self.hosts.view() {
            HostView::All => self.hosts.filtered(&self.book).len(),
            HostView::Groups => self
                .hosts
                .groups(&self.book)
                .iter()
                .map(shellune_storage::Group::len)
                .sum(),
            HostView::Recent => self.hosts.recent(&self.book).len(),
        };
        let mut list = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .debug_selector(move || format!("hosts.list.{entries}"));

        match self.hosts.view() {
            HostView::All => {
                let hosts = self.hosts.filtered(&self.book);
                if hosts.is_empty() {
                    return self.nothing_to_list(list);
                }
                for (index, host) in hosts.into_iter().enumerate() {
                    list = list.child(self.host_row(index, host, cx));
                }
            }
            HostView::Groups => {
                let groups = self.hosts.groups(&self.book);
                if groups.is_empty() {
                    return self.nothing_to_list(list);
                }
                // The row numbering comes from one function that runs across the groups,
                // so two rows of one list can never share an element id.
                let offsets = group_row_offsets(&groups);
                for (group_index, (group, offset)) in groups.iter().zip(offsets).enumerate() {
                    let label = group.label().unwrap_or(strings.hosts_ungrouped);
                    list = list.child(
                        div()
                            .debug_selector(move || format!("hosts.group.{group_index}"))
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .pt(metrics.scale(if group_index == 0 {
                                0.0
                            } else {
                                spacing::TIGHT
                            }))
                            .child(components::section_label(label.to_owned(), metrics))
                            .child(components::badge(group.len(), Tone::Neutral, metrics)),
                    );
                    for (row_index, host) in (offset..).zip(group.hosts()) {
                        list = list.child(self.host_row(row_index, host, cx));
                    }
                }
            }
            HostView::Recent => {
                let recent = self.hosts.recent(&self.book);
                if recent.is_empty() {
                    return list.child(self.hosts_recent_empty());
                }
                for (index, entry) in recent.into_iter().enumerate() {
                    list = list.child(self.host_recent_row(index, entry, cx));
                }
            }
        }
        list
    }

    /// One saved host with the buttons that change it.
    fn host_row(&self, index: usize, host: &HostConfig, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let id = host.id();
        let edited = host.clone();

        let mut row = components::row(metrics)
            .debug_selector(move || format!("hosts.row.{index}"))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .min_w(px(0.0))
                    .child(host_avatar(host.name(), false, metrics))
                    .child(components::stacked(
                        host.name().to_owned(),
                        host.target(),
                        metrics,
                    )),
            );
        if let Some(group) = host_group_chip(host) {
            row = row.child(components::tag(group, metrics));
        }
        let target = host.clone();
        row.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .child(self.icon_text_button(
                    SharedString::from(format!("hosts.connect.{index}")),
                    Icon::Plug,
                    strings.remote_connect,
                    Variant::Soft,
                    cx,
                    move |view, cx| view.open_remote(&target, cx),
                ))
                .child(self.text_button(
                    SharedString::from(format!("hosts.edit.{index}")),
                    strings.host_edit,
                    Variant::Ghost,
                    cx,
                    move |view, cx| view.open_edit_host(&edited, cx),
                ))
                .child(self.text_button(
                    SharedString::from(format!("hosts.delete.{index}")),
                    strings.host_delete,
                    Variant::Ghost,
                    cx,
                    move |view, cx| view.ask_delete_host(id, cx),
                )),
        )
    }

    /// One host that was really reached, with when it happened.
    fn host_recent_row(
        &self,
        index: usize,
        entry: shellune_storage::RecentHost<'_>,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let edited = entry.host.clone();
        let reconnect_target = entry.host.clone();
        let id = entry.host.id();
        let last = format!(
            "{} {} · {} {}",
            strings.host_last_connected,
            format_unix(entry.last_connected_unix),
            entry.connections,
            strings.host_connection_count
        );

        components::row(metrics)
            .debug_selector(move || format!("hosts.recent.{index}"))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .min_w(px(0.0))
                    .child(host_avatar(entry.host.name(), true, metrics))
                    .child(components::stacked(
                        entry.host.name().to_owned(),
                        format!("{} · {last}", entry.host.target()),
                        metrics,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.icon_text_button(
                        SharedString::from(format!("hosts.recent.connect.{index}")),
                        Icon::Plug,
                        strings.remote_connect,
                        Variant::Soft,
                        cx,
                        move |view, cx| view.open_remote(&reconnect_target, cx),
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("hosts.recent.edit.{index}")),
                        strings.host_edit,
                        Variant::Ghost,
                        cx,
                        move |view, cx| view.open_edit_host(&edited, cx),
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("hosts.recent.delete.{index}")),
                        strings.host_delete,
                        Variant::Ghost,
                        cx,
                        move |view, cx| view.ask_delete_host(id, cx),
                    )),
            )
    }

    /// Nothing to list: a search that matched nothing says so, while an empty
    /// book next to an open form just leaves the form to be filled.
    fn nothing_to_list(&self, list: Div) -> Div {
        if self.hosts.search().trim().is_empty() {
            list
        } else {
            list.child(self.hosts_no_match())
        }
    }

    fn hosts_no_match(&self) -> Div {
        let metrics = self.metrics();
        div()
            .debug_selector(|| "hosts.nomatch".to_owned())
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .child(self.strings().hosts_no_match)
    }

    fn hosts_recent_empty(&self) -> Div {
        let metrics = self.metrics();
        div()
            .debug_selector(|| "hosts.recent.empty".to_owned())
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .child(self.strings().hosts_recent_empty)
    }

    /// A small command button that runs one action.
    ///
    /// The variant carries the weight of the action, not its colour: a form's
    /// confirming button is [`Variant::Primary`], a removal is [`Variant::Danger`],
    /// and a toolbar command stays [`Variant::Ghost`] until it is pointed at.
    fn text_button(
        &self,
        id: SharedString,
        label: impl Into<SharedString>,
        variant: Variant,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let selector = id.to_string();
        components::compact_button(label, variant, metrics)
            .id(id)
            .debug_selector(move || selector.clone())
            .on_click(cx.listener(move |this, _event, _window, cx| click(this, cx)))
    }

    /// A small command button that leads with an icon.
    fn icon_text_button(
        &self,
        id: SharedString,
        icon: Icon,
        label: impl Into<SharedString>,
        variant: Variant,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let selector = id.to_string();
        components::icon_label_button(icon, label, variant, metrics)
            .h(metrics.compact_control_height())
            .pl(metrics.scale(spacing::TIGHT))
            .pr(metrics.scale(spacing::TIGHT + 2.0))
            .text_size(metrics.caption_size())
            .id(id)
            .debug_selector(move || selector.clone())
            .on_click(cx.listener(move |this, _event, _window, cx| click(this, cx)))
    }

    /// The product mark: a gradient tile with a prompt, the name and how it is said.
    fn brand(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        // The macOS window buttons sit at the left of this row.
        let left = if Platform::current() == Platform::MacOs {
            titlebar::TRAFFIC_LIGHT_ROOM
        } else {
            spacing::BASE - 2.0
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(metrics.header_height())
            .pr(metrics.scale(spacing::TIGHT))
            .child(
                self.drag_area("brand.drag")
                    .flex()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG - 2.0))
                    .min_w(px(0.0))
                    .pl(metrics.scale(left))
                    .when(Platform::current() != Platform::MacOs, |row| {
                        row.child(brand_mark(metrics.scale(28.0), metrics))
                    })
                    .child(
                        // The pronunciation is one hover away: the row keeps room for the
                        // sidebar button without cutting the name.
                        div()
                            .id("brand.name")
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(metrics.display_size())
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .tooltip(components::tooltip(SharedString::from(format!(
                                "{} · {}",
                                product::NAME,
                                product::PRONUNCIATION
                            ))))
                            .child(product::NAME),
                    ),
            )
            .child(self.rail_toggle(cx))
    }

    fn nav_entry(
        &self,
        destination: Destination,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let state = self.workspace.state(destination);
        let selected = self.workspace.destination() == destination;
        let focused = self.destination_focus(destination).is_focused(window);
        // The dot only appears when the destination has something to say: a live
        // session, a failure, a load in flight. An idle entry stays quiet.
        let tone = match state {
            SectionState::Populated => self.destination_tone(destination),
            state => state.tone(),
        };

        let index = destination.index();
        let count = self.badge_count(destination);
        let hover = theme::swatch(palette::SURFACE_HOVER);
        let hover_text = theme::swatch(palette::TEXT_PRIMARY);
        let group = SharedString::from(format!("nav-{index}"));
        let tooltip = format!(
            "{} · {}",
            destination.label(strings),
            destination.shortcut_label(Platform::current())
        );

        let collapsed = self.rail_collapsed;
        let mut entry = div()
            .id(("nav", index))
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::SNUG - 2.0))
            .h(metrics.nav_row_height())
            .when(collapsed, |entry| entry.justify_center())
            .when(!collapsed, |entry| {
                entry
                    .pl(metrics.scale(spacing::SNUG - 2.0))
                    .pr(metrics.scale(spacing::TIGHT))
            })
            .rounded(px(radius::MEDIUM))
            .border_1()
            // The ring marks keyboard focus that has moved off the current page; on
            // the selected entry the fill already says where focus is.
            .border_color(if focused && !selected {
                theme::swatch(palette::ACCENT)
            } else {
                theme::clear()
            })
            .text_size(metrics.body_size())
            .cursor_pointer()
            .track_focus(self.destination_focus(destination))
            .debug_selector(move || destination.debug_selector().to_owned())
            .tooltip(components::tooltip(tooltip))
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.select(Destination::ALL[index], window, cx);
            }));
        let icon_color = if selected {
            theme::swatch(palette::ACCENT)
        } else {
            theme::swatch(palette::TEXT_MUTED)
        };
        entry = if selected {
            entry
                .bg(theme::swatch(palette::SURFACE_SELECTED))
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .font_weight(FontWeight::MEDIUM)
        } else {
            entry
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .hover(move |style| style.bg(hover).text_color(hover_text))
        };

        let live = !matches!(tone, Tone::Neutral | Tone::Planned);
        let icon = icons::icon(
            destination_icon(destination),
            metrics.icon_size(),
            icon_color,
        )
        .group_hover(group, move |style| style.text_color(hover_text));
        if collapsed {
            // Only the icon, with the state as a dot on its corner; the name and the
            // shortcut are in the tooltip.
            return entry.child(icon).when(live, |entry| {
                entry.child(
                    div()
                        .absolute()
                        .top(metrics.scale(6.0))
                        .right(metrics.scale(6.0))
                        .child(components::status_dot(tone, metrics)),
                )
            });
        }
        entry
            .child(icon)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(destination.label(strings)),
            )
            .when(live, |entry| {
                entry.child(components::status_dot(tone, metrics))
            })
            .when(count > 0, |entry| {
                entry.child(components::badge(
                    count,
                    if selected {
                        Tone::Accent
                    } else {
                        Tone::Neutral
                    },
                    metrics,
                ))
            })
    }

    /// The number beside a destination in the sidebar.
    ///
    /// For the session pages it counts what is still open, so it matches the tabs: a
    /// closed session stays in the list as a record, but it is no longer something the
    /// user has running.
    fn badge_count(&self, destination: Destination) -> usize {
        let open = |session: &&Session| !matches!(session.state(), SessionState::Closed);
        match destination {
            Destination::LocalShells => self.workspace.local_sessions().filter(open).count(),
            Destination::Sessions => self
                .workspace
                .sessions()
                .items()
                .iter()
                .filter(open)
                .count(),
            Destination::Hosts | Destination::Transfers => self.workspace.item_count(destination),
        }
    }

    /// The tone a populated destination's dot is drawn in.
    ///
    /// A list of sessions is green only while one of them is live; a list where every
    /// session ended says so instead of looking healthy, and a transfer in flight shows
    /// as work in progress.
    fn destination_tone(&self, destination: Destination) -> Tone {
        let sessions: Vec<&Session> = match destination {
            Destination::LocalShells => self.workspace.local_sessions().collect(),
            Destination::Sessions => self.workspace.sessions().items().iter().collect(),
            Destination::Transfers => {
                let running = self
                    .workspace
                    .transfers()
                    .items()
                    .iter()
                    .any(Transfer::is_active);
                return if running {
                    Tone::Warning
                } else {
                    Tone::Neutral
                };
            }
            Destination::Hosts => return Tone::Neutral,
        };
        if sessions
            .iter()
            .any(|session| session.state() == &SessionState::Connected)
        {
            Tone::Positive
        } else if sessions
            .iter()
            .any(|session| session_tone(session.state()) == Tone::Warning)
        {
            Tone::Warning
        } else {
            Tone::Neutral
        }
    }

    fn rail(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut nav = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(2.0))
            .px(metrics.scale(if self.rail_collapsed {
                spacing::TIGHT
            } else {
                spacing::TIGHT + 2.0
            }))
            .pt(metrics.scale(spacing::TIGHT));
        for destination in Destination::ALL {
            nav = nav.child(self.nav_entry(destination, window, cx));
        }

        if self.rail_collapsed {
            return self.collapsed_rail(nav, cx);
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(metrics.rail_width())
            .h_full()
            .bg(theme::swatch(palette::SURFACE))
            .border_r_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "rail".to_owned())
            .child(self.brand(cx))
            .child(self.palette_trigger(cx))
            .child(nav)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(2.0))
                    .px(metrics.scale(spacing::TIGHT + 2.0))
                    .pb(metrics.scale(spacing::SNUG))
                    .mt_auto()
                    .child(
                        div()
                            .mx(metrics.scale(spacing::HAIRLINE))
                            .mb(metrics.scale(spacing::TIGHT))
                            .h(px(1.0))
                            .bg(theme::swatch(palette::BORDER)),
                    )
                    .child(
                        self.rail_control(
                            self.theme_icon(),
                            strings.theme_control,
                            self.theme_label(),
                        )
                        .id("theme.control")
                        .debug_selector(|| "theme.control".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.cycle_theme(cx);
                            },
                        )),
                    )
                    // The language control is clickable because the shortcut it
                    // duplicates cannot be: inside a terminal Ctrl+L is the shell's key.
                    .child(
                        self.rail_control(
                            Icon::Globe,
                            strings.language_control,
                            self.workspace.locale().endonym(),
                        )
                        .id("status.language")
                        .debug_selector(|| "status.language".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.toggle_locale(cx);
                            },
                        )),
                    )
                    .child(
                        self.rail_control(
                            Icon::TextSize,
                            strings.zoom_control,
                            self.workspace.zoom().label(),
                        )
                        .id("zoom.control")
                        .debug_selector(|| "zoom.control".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.workspace.cycle_text_size();
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                            .px(metrics.scale(spacing::SNUG - 2.0))
                            .pt(metrics.scale(spacing::TIGHT))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(components::status_dot(
                                if self.any_live() {
                                    Tone::Positive
                                } else {
                                    Tone::Neutral
                                },
                                metrics,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .child(SharedString::from(self.activity_summary())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .debug_selector(|| "status.version".to_owned())
                                    .child(SharedString::from(format!("v{}", self.version()))),
                            ),
                    ),
            )
    }

    /// The search box at the top of the sidebar, which opens the command palette.
    ///
    /// It looks like a field because that is what it leads to, and it names its
    /// shortcut, so a user who clicks it once learns how not to next time.
    fn palette_trigger(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let hover_border = theme::swatch(palette::BORDER_STRONG);
        let hover_text = theme::swatch(palette::TEXT_SECONDARY);
        div()
            .id("palette.open")
            .debug_selector(|| "palette.open".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .mx(metrics.scale(spacing::TIGHT + 2.0))
            .mb(metrics.scale(spacing::HAIRLINE))
            .h(metrics.scale(30.0))
            .pl(metrics.scale(spacing::TIGHT + 1.0))
            .pr(metrics.scale(spacing::HAIRLINE + 1.0))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .bg(theme::swatch(palette::CANVAS))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .cursor_pointer()
            .hover(move |style| style.border_color(hover_border).text_color(hover_text))
            .on_click(cx.listener(|this, _event, window, cx| {
                this.toggle_palette(window, cx);
            }))
            .child(icons::icon(
                Icon::Search,
                metrics.small_icon_size(),
                theme::swatch(palette::TEXT_MUTED),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(strings.palette_open),
            )
            .child(components::keycap(
                Platform::current().palette_label(),
                metrics,
            ))
    }

    /// Steps the theme: system, light, dark.
    pub fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme_choice = self.theme_choice.next();
        self.repaint_terminals(cx);
        cx.notify();
    }

    /// The theme the user chose.
    #[allow(dead_code)]
    #[must_use]
    pub fn theme_choice(&self) -> theme::ThemeChoice {
        self.theme_choice
    }

    /// Asks every terminal to draw again, so a new palette reaches the ones that are
    /// not on screen too.
    fn repaint_terminals(&self, cx: &mut Context<Self>) {
        for open in &self.panes {
            open.pane.update(cx, |_pane, cx| cx.notify());
        }
    }

    /// The name of the current theme choice.
    fn theme_label(&self) -> &'static str {
        let strings = self.strings();
        match self.theme_choice {
            theme::ThemeChoice::System => strings.theme_system,
            theme::ThemeChoice::Light => strings.theme_light,
            theme::ThemeChoice::Dark => strings.theme_dark,
        }
    }

    /// The icon of the current theme choice.
    fn theme_icon(&self) -> Icon {
        match self.theme_choice {
            theme::ThemeChoice::System => Icon::Monitor,
            theme::ThemeChoice::Light => Icon::Sun,
            theme::ThemeChoice::Dark => Icon::Moon,
        }
    }

    /// Collapses the sidebar to its icons, or expands it again.
    pub fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        self.rail_collapsed = !self.rail_collapsed;
        cx.notify();
    }

    /// Whether the sidebar is collapsed.
    #[allow(dead_code)]
    #[must_use]
    pub fn rail_collapsed(&self) -> bool {
        self.rail_collapsed
    }

    /// The button that collapses or expands the sidebar, with its shortcut.
    fn rail_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let label = if self.rail_collapsed {
            strings.sidebar_expand
        } else {
            strings.sidebar_collapse
        };
        components::icon_only_button(Icon::Sidebar, metrics)
            .id("rail.toggle")
            .debug_selector(|| "rail.toggle".to_owned())
            .tooltip(components::tooltip(SharedString::from(format!(
                "{label} ({})",
                Platform::current().sidebar_label()
            ))))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.toggle_rail(cx);
            }))
    }

    /// The sidebar collapsed to its icons: the brand, search, the pages and the two
    /// settings, each named in its tooltip.
    fn collapsed_rail(&self, nav: Div, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let platform = Platform::current();
        let square = |icon: Icon, id: &'static str, tooltip: String| {
            components::icon_only_button(icon, metrics)
                .size(metrics.scale(36.0))
                .id(id)
                .debug_selector(move || id.to_owned())
                .tooltip(components::tooltip(SharedString::from(tooltip)))
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(if Platform::current() == Platform::MacOs {
                metrics.scale(titlebar::TRAFFIC_LIGHT_ROOM + 4.0)
            } else {
                metrics.collapsed_rail_width()
            })
            .h_full()
            .bg(theme::swatch(palette::SURFACE))
            .border_r_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "rail".to_owned())
            .child(
                self.drag_area("brand.drag")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .h(metrics.header_height())
                    // On macOS the window buttons take this place.
                    .when(Platform::current() != Platform::MacOs, |row| {
                        row.child(brand_mark(metrics.scale(28.0), metrics))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(metrics.scale(2.0))
                    .child(self.rail_toggle(cx).size(metrics.scale(36.0)))
                    .child(
                        square(
                            Icon::Search,
                            "palette.open",
                            format!(
                                "{} ({})",
                                strings.palette_hint_short,
                                platform.palette_label()
                            ),
                        )
                        .on_click(cx.listener(
                            |this, _event, window, cx| {
                                this.toggle_palette(window, cx);
                            },
                        )),
                    ),
            )
            .child(nav)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(metrics.scale(2.0))
                    .pb(metrics.scale(spacing::SNUG))
                    .mt_auto()
                    .child(
                        square(
                            self.theme_icon(),
                            "theme.control",
                            format!("{} · {}", strings.theme_control, self.theme_label()),
                        )
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.cycle_theme(cx);
                            },
                        )),
                    )
                    .child(
                        square(
                            Icon::Globe,
                            "status.language",
                            format!(
                                "{} · {}",
                                strings.language_control,
                                self.workspace.locale().endonym()
                            ),
                        )
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.toggle_locale(cx);
                            },
                        )),
                    )
                    .child(
                        square(
                            Icon::TextSize,
                            "zoom.control",
                            format!(
                                "{} · {}",
                                strings.zoom_control,
                                self.workspace.zoom().label()
                            ),
                        )
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.workspace.cycle_text_size();
                                cx.notify();
                            },
                        )),
                    )
                    .child(div().pt(metrics.scale(spacing::HAIRLINE)).child(
                        components::status_dot(
                            if self.any_live() {
                                Tone::Positive
                            } else {
                                Tone::Neutral
                            },
                            metrics,
                        ),
                    )),
            )
    }

    /// One quiet setting at the foot of the sidebar: an icon, a name and its value.
    fn rail_control(&self, icon: Icon, label: &'static str, value: &'static str) -> Div {
        let metrics = self.metrics();
        let hover = theme::swatch(palette::SURFACE_HOVER);
        let hover_text = theme::swatch(palette::TEXT_PRIMARY);
        div()
            .group(components::ICON_GROUP)
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::SNUG - 2.0))
            .h(metrics.scale(30.0))
            .pl(metrics.scale(spacing::SNUG - 2.0))
            .pr(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .cursor_pointer()
            .hover(move |style| style.bg(hover).text_color(hover_text))
            .child(
                icons::icon(
                    icon,
                    metrics.small_icon_size(),
                    theme::swatch(palette::TEXT_MUTED),
                )
                .group_hover(components::ICON_GROUP, move |style| {
                    style.text_color(hover_text)
                }),
            )
            .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
            .child(components::keycap(value, metrics))
    }

    /// Whether any session is live right now.
    fn any_live(&self) -> bool {
        self.workspace
            .sessions()
            .items()
            .iter()
            .any(|session| session.state() == &SessionState::Connected)
    }

    /// The non-zero counts, or that nothing is running, in one short line.
    fn activity_summary(&self) -> String {
        let strings = self.strings();
        let locale: Locale = self.workspace.locale();
        let parts: Vec<String> = [
            (
                self.badge_count(Destination::Sessions),
                strings.count_sessions,
            ),
            (self.workspace.transfers().count(), strings.count_transfers),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| copy::counted(count, unit, locale))
        .collect();
        if parts.is_empty() {
            strings.idle.to_owned()
        } else {
            parts.join(" · ")
        }
    }

    fn notice_strip(&self, cx: &mut Context<Self>) -> Option<Div> {
        let notice = self.workspace.notice()?;
        let metrics = self.metrics();
        let strings = self.strings();
        Some(
            div()
                .flex_none()
                .px(metrics.scale(spacing::LOOSE - 4.0))
                .pt(metrics.scale(spacing::SNUG))
                .debug_selector(|| "notice".to_owned())
                .child(
                    components::banner(Tone::Warning, metrics)
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .child(notice.text(strings)),
                        )
                        .child(
                            components::icon_only_button(Icon::Close, metrics)
                                .id("notice.dismiss")
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.workspace.dismiss_notice();
                                    cx.notify();
                                })),
                        ),
                ),
        )
    }

    /// Whether the header offers the shell menu beside its main button.
    ///
    /// The menu replaces the shell tiles once a terminal fills the panel, and on the
    /// sessions page, whose main button opens a local shell as well.
    fn offers_shell_menu(&self) -> bool {
        match self.workspace.destination() {
            Destination::LocalShells => self.shows_terminal(Destination::LocalShells),
            Destination::Sessions => true,
            Destination::Hosts | Destination::Transfers => false,
        }
    }

    /// The label of the header's main button, which can depend on what is open.
    fn header_primary_label(&self, destination: Destination) -> &'static str {
        let strings = self.strings();
        match destination {
            Destination::Transfers if self.browsable_session().is_none() => {
                strings.primary_transfers_connect
            }
            destination => self.primary_label(destination, strings),
        }
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let destination = self.workspace().destination();
        let state = self.workspace().state(destination);
        // With a terminal on screen the header is the tab bar: the page title would
        // only repeat what the tabs and the sidebar already say.
        let terminal_mode = self.shows_terminal(destination);

        // What the page holds, said once: the summary, and the amount when there is one.
        let subtitle = if state == SectionState::Populated {
            format!(
                "{} · {}",
                destination.summary(strings),
                copy::counted(
                    self.workspace.item_count(destination),
                    destination.count_unit(strings),
                    self.workspace.locale()
                )
            )
        } else {
            destination.summary(strings).to_owned()
        };

        let mut title_row = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .min_w(px(0.0))
            .child(
                div()
                    .text_size(metrics.heading_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .truncate()
                    .debug_selector(|| "header.title".to_owned())
                    .child(destination.label(strings)),
            );
        // A ready page needs no badge to say so; loading and failure do, because they
        // change what the content below can be trusted for.
        if matches!(state, SectionState::Loading | SectionState::Failed) {
            title_row = title_row.child(components::chip(
                state.chip_label(strings),
                state.tone(),
                metrics,
            ));
        }

        let menu = self.offers_shell_menu();
        let open = self.shell_menu_open;
        let primary_label = self.header_primary_label(destination);
        let primary_icon = match destination {
            Destination::LocalShells | Destination::Sessions => Icon::Plus,
            Destination::Hosts => Icon::Plus,
            Destination::Transfers => {
                if self.browsable_session().is_some() {
                    Icon::Folder
                } else {
                    Icon::Server
                }
            }
        };
        // The main button and the shell menu form one split control, so the default
        // shell and "pick another" read as two halves of the same action.
        let mut primary =
            components::icon_label_button(primary_icon, primary_label, Variant::Primary, metrics);
        if menu {
            primary = primary.rounded_r(px(0.0));
        }
        let mut actions = div().relative().flex().flex_none().items_center().child(
            div()
                .id("header.primary")
                .debug_selector(|| "header.primary".to_owned())
                .when(terminal_mode, |control| {
                    control.tooltip(components::tooltip(SharedString::from(format!(
                        "{primary_label} ({})",
                        new_shell_shortcut_label(Platform::current())
                    ))))
                })
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.request_primary(window, cx);
                }))
                .child(primary),
        );
        if menu {
            let hover = theme::swatch(palette::ACCENT_HOVER);
            actions = actions.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(metrics.scale(28.0))
                    .h(metrics.control_height())
                    .rounded_r(px(radius::MEDIUM))
                    .border_1()
                    .border_l_1()
                    .border_color(theme::swatch(palette::ACCENT_HOVER))
                    .bg(theme::swatch(if open {
                        palette::ACCENT_HOVER
                    } else {
                        palette::ACCENT
                    }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(
                        div()
                            .w(px(1.0))
                            .h(metrics.scale(16.0))
                            .absolute()
                            .left(px(-1.0))
                            .bg(theme::faded(theme::swatch(palette::ON_ACCENT), 0.25)),
                    )
                    .child(icons::icon(
                        Icon::ChevronDown,
                        metrics.small_icon_size(),
                        theme::swatch(palette::ON_ACCENT),
                    ))
                    .id("shell.menu")
                    .debug_selector(|| "shell.menu".to_owned())
                    .when(!open, |button| {
                        button.tooltip(components::tooltip(strings.shell_menu_hint))
                    })
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.shell_menu_open = !this.shell_menu_open;
                        cx.notify();
                    })),
            );
            if open {
                actions = actions.child(self.shell_menu(window, cx));
            }
        }

        // The blank part of the row is the window's caption: it drags the window. The
        // tabs and buttons stay outside it, so a press on them is never a drag.
        let leading = if terminal_mode {
            div()
                .id("header.leading")
                .flex()
                .flex_1()
                .h_full()
                .items_center()
                .min_w(px(0.0))
                .child(self.tab_row(cx).flex_initial())
                .child(self.drag_area("header.drag").flex_1().h_full())
        } else {
            self.drag_area("header.drag")
                .flex()
                .flex_1()
                .h_full()
                .items_center()
                .min_w(px(0.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(metrics.scale(1.0))
                        .min_w(px(0.0))
                        .child(title_row)
                        .child(
                            div()
                                .truncate()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .child(SharedString::from(subtitle)),
                        ),
                )
        };
        let controls = self.window_controls(window, cx);

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::BASE))
            .h(metrics.header_height())
            .flex_none()
            .pl(metrics.scale(if terminal_mode {
                spacing::TIGHT
            } else {
                spacing::LOOSE
            }))
            .pr(if controls.is_some() {
                px(0.0)
            } else {
                metrics.scale(spacing::SNUG)
            })
            .bg(theme::swatch(if terminal_mode {
                palette::SURFACE
            } else {
                palette::CANVAS
            }))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(leading)
            .child(actions)
            .children(controls)
    }

    /// The menu under the header's shell control: every shell, and a typed path.
    ///
    /// Picking a shell opens it in a new tab at once, which is what the control is
    /// for; the choice is also remembered as the one the main button opens next. A
    /// transparent layer behind the menu closes it on any click outside.
    fn shell_menu(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let viewport = window.viewport_size();

        let mut menu = div()
            .flex()
            .flex_col()
            .w(metrics.scale(300.0))
            .p(metrics.scale(spacing::HAIRLINE + 1.0))
            .gap(metrics.scale(2.0))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
            .debug_selector(|| "shell.picker".to_owned())
            .child(
                components::section_label(strings.shell_launch_title, metrics)
                    .px(metrics.scale(spacing::TIGHT))
                    .pt(metrics.scale(spacing::HAIRLINE))
                    .pb(metrics.scale(2.0)),
            );

        if self.workspace.shells().is_empty() {
            menu = menu.child(
                div()
                    .px(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Warning.text())
                    .child(strings.shell_none_found),
            );
        }
        for (index, shell) in self.workspace.shells().iter().enumerate() {
            let selected = index == self.workspace.shell_choice();
            menu = menu.child(
                self.menu_item(
                    shell.name().to_owned(),
                    Some(program_label(shell.program())),
                    selected,
                )
                .id(("shell.choice", index))
                .debug_selector(move || format!("shell.choice.{index}"))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.launch_choice(index, window, cx);
                })),
            );
        }
        menu = menu
            .child(
                div()
                    .h(px(1.0))
                    .mx(metrics.scale(spacing::HAIRLINE))
                    .my(metrics.scale(spacing::HAIRLINE))
                    .bg(theme::swatch(palette::BORDER)),
            )
            .child(
                self.menu_item(
                    format!("{}{ELLIPSIS}", strings.shell_custom_button),
                    None,
                    self.custom_shell_open,
                )
                .id("shell.custom")
                .debug_selector(|| "shell.custom".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.toggle_custom_shell(window, cx);
                })),
            )
            .child(
                self.menu_item(
                    strings.quick_connect.to_owned(),
                    Some(strings.quick_connect_hint.to_owned()),
                    false,
                )
                .id("menu.quick")
                .debug_selector(|| "menu.quick".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.open_quick_connect(window, cx);
                })),
            )
            .child(
                self.menu_item(strings.primary_hosts.to_owned(), None, false)
                    .id("menu.new_host")
                    .debug_selector(|| "menu.new_host".to_owned())
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.shell_menu_open = false;
                        this.select(Destination::Hosts, window, cx);
                        this.open_create_host(cx);
                    })),
            )
            .child(
                self.menu_item(strings.import_open.to_owned(), None, false)
                    .id("menu.import")
                    .debug_selector(|| "menu.import".to_owned())
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.shell_menu_open = false;
                        this.select(Destination::Hosts, window, cx);
                        this.open_import(cx);
                    })),
            );
        if self.custom_shell_open {
            menu = menu.child(
                div()
                    .p(metrics.scale(spacing::HAIRLINE))
                    .debug_selector(|| "shell.path".to_owned())
                    .child(self.shell_path.clone()),
            );
        }

        div()
            .child(gpui::deferred(
                gpui::anchored().position(gpui::Point::default()).child(
                    div()
                        .id("shell.menu.backdrop")
                        .w(viewport.width)
                        .h(viewport.height)
                        .occlude()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _event, _window, cx| {
                                this.close_shell_menu(cx);
                            }),
                        ),
                ),
            ))
            .child(
                div()
                    .absolute()
                    .top(metrics.control_height() + metrics.scale(spacing::HAIRLINE + 2.0))
                    .right(px(0.0))
                    .child(
                        gpui::deferred(
                            gpui::anchored()
                                .anchor(gpui::Corner::TopRight)
                                .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                                .child(div().occlude().child(menu)),
                        )
                        .with_priority(1),
                    ),
            )
    }

    /// One line of a menu: a name, an optional detail and whether it is the current one.
    fn menu_item(&self, label: String, detail: Option<String>, selected: bool) -> Div {
        let metrics = self.metrics();
        let hover = theme::swatch(palette::SURFACE_HOVER);
        let custom = detail.is_none();
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .h(metrics.scale(34.0))
            .px(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .cursor_pointer()
            .when(selected, |item| {
                item.bg(theme::swatch(palette::SURFACE_HOVER))
            })
            .hover(move |style| style.bg(hover))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(metrics.scale(24.0))
                    .h(metrics.scale(24.0))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(if custom {
                        palette::SURFACE_SELECTED
                    } else {
                        palette::ACCENT_TINT
                    }))
                    .child(icons::icon(
                        if custom { Icon::Search } else { Icon::Terminal },
                        metrics.small_icon_size(),
                        theme::swatch(if custom {
                            palette::TEXT_SECONDARY
                        } else {
                            palette::ACCENT
                        }),
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .when(selected, |label| label.font_weight(FontWeight::MEDIUM))
                    .child(SharedString::from(label)),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_right()
                    .font_family(terminal::monospace_family())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(detail))
            }))
    }

    /// Closes the shell menu, and with it the path field inside it.
    fn close_shell_menu(&mut self, cx: &mut Context<Self>) {
        if self.shell_menu_open {
            self.shell_menu_open = false;
            cx.notify();
        }
    }

    /// Opens or closes the typed-path field, giving it the keyboard when it opens.
    fn toggle_custom_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.custom_shell_open = !self.custom_shell_open;
        if self.custom_shell_open {
            let handle = self.shell_path.read(cx).focus_handle().clone();
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Remembers one shell as the default and opens it in a new tab.
    fn launch_choice(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.select_shell(index);
        // A choice is not a typed path: the field closes so its text cannot win.
        self.custom_shell_open = false;
        self.shell_menu_open = false;
        self.start_from_picker(window, cx);
        cx.notify();
    }

    /// The first remote session whose connection can list files, if any.
    fn browsable_session(&self) -> Option<SessionId> {
        self.tab_sessions()
            .into_iter()
            .find(|id| self.files.can_browse(*id))
    }

    /// The card a section shows when it holds no rows.
    fn section_card(&self, destination: Destination, state: SectionState) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        match state {
            SectionState::Loading => components::state_card(
                Tone::Warning,
                strings.loading_title,
                strings.loading_body,
                None,
                metrics,
            ),
            SectionState::Failed => {
                let failure = self.workspace.failure(destination);
                components::state_card(
                    Tone::Danger,
                    strings.failed_title,
                    failure.map_or_else(
                        || strings.loading_body.to_owned(),
                        |failure| failure.reason().to_owned(),
                    ),
                    Some(SharedString::from(format!(
                        "{} · {}",
                        strings.recovery_label,
                        failure.map_or(strings.recovery_retry, |failure| failure
                            .recovery()
                            .label(strings))
                    ))),
                    metrics,
                )
            }
            SectionState::Empty => components::state_card_with_icon(
                Tone::Neutral,
                Some(destination_icon(destination)),
                match destination {
                    Destination::LocalShells => strings.empty_local_title,
                    Destination::Hosts => strings.empty_hosts_title,
                    Destination::Sessions => strings.empty_sessions_title,
                    Destination::Transfers => strings.empty_transfers_title,
                },
                match destination {
                    Destination::LocalShells => strings.empty_local_body,
                    Destination::Hosts => strings.empty_hosts_body,
                    Destination::Sessions => strings.empty_sessions_body,
                    Destination::Transfers => strings.empty_transfers_body,
                },
                None,
                metrics,
            ),
            SectionState::Populated => div().flex().flex_col().gap(metrics.scale(spacing::TIGHT)),
        }
    }

    fn rows_for(
        &self,
        destination: Destination,
        list: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match destination {
            Destination::LocalShells => self.local_rows(list, cx),
            // The hosts destination renders through `hosts_panel`; this arm keeps
            // the list untouched if a future layout routes it here.
            Destination::Hosts => list,
            Destination::Sessions => self.session_rows(list, cx),
            Destination::Transfers => self.transfer_rows(list),
        }
    }

    /// Whether a destination shows the selected terminal instead of a list.
    ///
    /// Local and remote sessions share the workspace. The kind is written on the
    /// session itself, not by sending the user to a different page. Hosts and
    /// transfers stay context panels and do not hide the fact that a session exists.
    fn shows_terminal(&self, destination: Destination) -> bool {
        if self.showing_connection_form
            && self.remote.is_some()
            && destination != Destination::Hosts
        {
            return false;
        }
        let Some(id) = self.active else {
            return false;
        };
        if self.pane_for(id).is_none() {
            return false;
        }
        match destination {
            Destination::LocalShells => true,
            Destination::Sessions => self.session_terminal,
            Destination::Hosts | Destination::Transfers => false,
        }
    }

    fn destination_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let destination = self.workspace.destination();
        let state = self.workspace.state(destination);

        // A terminal fills the panel edge to edge; everything else is a reading
        // column that scrolls. A connection form opened from the workspace replaces
        // the terminal until the user picks a session tab again; the attempts behind
        // it keep running.
        if destination != Destination::Hosts
            && self.showing_connection_form
            && self.remote.is_some()
        {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .id("content.scroll")
                .debug_selector(|| "content".to_owned())
                .overflow_y_scroll()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_w(metrics.content_max_width())
                        .mx_auto()
                        .px(metrics.scale(spacing::LOOSE))
                        .py(metrics.scale(spacing::LOOSE))
                        .gap(metrics.panel_gap())
                        .children(self.quick_open.then(|| self.quick_panel(cx)))
                        .child(self.remote_panel(cx))
                        .child(self.other_attempts(cx)),
                );
        }
        if self.shows_terminal(destination) {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .id("content.scroll")
                .debug_selector(|| "content".to_owned())
                .children(self.quick_open.then(|| {
                    div()
                        .flex_none()
                        .px(metrics.scale(spacing::SNUG))
                        .pt(metrics.scale(spacing::TIGHT))
                        .child(self.quick_panel(cx))
                }))
                .child(self.terminal_panel(cx));
        }

        // The first page a new user sees is a welcome, not a report: the shells are
        // the call to action, laid out in full.
        if destination == Destination::LocalShells && state == SectionState::Empty {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .id("content.scroll")
                .debug_selector(|| "content".to_owned())
                .overflow_y_scroll()
                .child(self.shell_launcher(cx));
        }

        let body = if destination == Destination::Hosts {
            // The hosts destination renders its own empty, failed and populated
            // states: the tools, the form and the list are one panel.
            self.hosts_destination(cx)
        } else {
            let card = self.section_card(destination, state);
            if state == SectionState::Populated {
                self.rows_for(destination, card, cx)
            } else {
                card
            }
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .id("content.scroll")
            .debug_selector(|| "content".to_owned())
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(metrics.content_max_width())
                    .mx_auto()
                    .px(metrics.scale(spacing::LOOSE))
                    .py(metrics.scale(spacing::LOOSE))
                    .gap(metrics.panel_gap())
                    .child(body),
            )
    }

    /// The welcome page: the shells this machine offers, as tiles that open one in a
    /// click (R2), and the ways to go further.
    ///
    /// The empty local page is where a first shell is started, so the choices are
    /// laid out in full instead of hiding in a menu; the tile that is the default
    /// for the header's button is marked.
    fn shell_launcher(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let platform = Platform::current();
        let mut tiles = div()
            .flex()
            .flex_wrap()
            .w_full()
            .justify_center()
            .gap(metrics.scale(spacing::SNUG));

        for (index, shell) in self.workspace.shells().iter().enumerate() {
            let selected = index == self.workspace.shell_choice();
            tiles = tiles.child(
                self.shell_tile(
                    shell.name().to_owned(),
                    Some(program_label(shell.program())),
                    selected,
                )
                .id(("shell.choice", index))
                .debug_selector(move || format!("shell.choice.{index}"))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.launch_choice(index, window, cx);
                })),
            );
        }
        tiles = tiles.child(
            self.shell_tile(
                format!("{}{ELLIPSIS}", strings.shell_custom_button),
                None,
                self.custom_shell_open,
            )
            .id("shell.custom")
            .debug_selector(|| "shell.custom".to_owned())
            .on_click(cx.listener(|this, _event, window, cx| {
                this.toggle_custom_shell(window, cx);
            })),
        );

        let hero = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .child(brand_mark(metrics.scale(52.0), metrics).rounded(px(radius::XLARGE - 2.0)))
            .child(
                div()
                    .mt(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.hero_size())
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .text_center()
                    .child(strings.shell_launch_title),
            )
            .child(
                div()
                    .max_w(metrics.scale(440.0))
                    .text_size(metrics.body_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .text_center()
                    .child(match self.workspace.locale() {
                        Locale::English => product::TAGLINE_EN,
                        Locale::Chinese => product::TAGLINE_ZH,
                    }),
            );

        let mut launcher = div().flex().flex_col().items_center();
        // The form is the thing the user just asked for, so it leads. Leaving it under
        // the tiles centres it out of a short window, where its button cannot be reached.
        if self.quick_open {
            launcher = launcher.child(self.quick_panel(cx));
        }
        launcher = launcher
            .w_full()
            .max_w(metrics.scale(720.0))
            .gap(metrics.scale(spacing::LOOSE - 4.0))
            .debug_selector(|| "shell.picker".to_owned())
            .child(hero);
        if self.workspace.shells().is_empty() {
            launcher = launcher.child(
                components::banner(Tone::Warning, metrics)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(strings.shell_none_found),
            );
        }
        launcher = launcher.child(tiles);
        if self.custom_shell_open {
            launcher = launcher.child(
                div()
                    .w_full()
                    .max_w(metrics.scale(480.0))
                    .debug_selector(|| "shell.path".to_owned())
                    .child(self.shell_path.clone()),
            );
        }

        let hint = |keys: String, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                .child(components::keycap(keys, metrics))
                .child(label)
        };
        launcher = launcher
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .mt(metrics.scale(spacing::HAIRLINE))
                    .justify_center()
                    .items_center()
                    .gap(metrics.scale(spacing::LOOSE - 4.0))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(hint(
                        new_shell_shortcut_label(platform).to_owned(),
                        strings.primary_local,
                    ))
                    .child(hint(platform.shortcut_range(), strings.nav_hint_short))
                    .child(hint(
                        platform.palette_label().to_owned(),
                        strings.palette_hint_short,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::icon_label_button(
                            Icon::Plug,
                            strings.quick_connect,
                            Variant::Secondary,
                            metrics,
                        )
                        .id("welcome.quick")
                        .debug_selector(|| "welcome.quick".to_owned())
                        .tooltip(components::tooltip(strings.quick_connect_hint))
                        .on_click(cx.listener(
                            |this, _event, window, cx| {
                                this.open_quick_connect(window, cx);
                            },
                        )),
                    )
                    .child(
                        components::icon_label_button(
                            Icon::Server,
                            strings.welcome_hosts,
                            Variant::Ghost,
                            metrics,
                        )
                        .id("welcome.hosts")
                        .debug_selector(|| "welcome.hosts".to_owned())
                        .on_click(cx.listener(
                            |this, _event, window, cx| {
                                this.select(Destination::Hosts, window, cx);
                            },
                        )),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .items_center()
            .when(self.quick_open, |page| page.justify_start())
            .when(!self.quick_open, |page| page.justify_center())
            .flex_1()
            .min_h_full()
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::LOOSE))
            .child(launcher.w_full())
    }

    /// One launcher tile: the shell's name and the program it runs.
    fn shell_tile(&self, name: String, program: Option<String>, selected: bool) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let hover = theme::swatch(palette::SURFACE_HOVER);
        let hover_border = theme::swatch(palette::BORDER_STRONG);
        let custom = program.is_none();
        let emblem_color = if custom {
            theme::swatch(palette::TEXT_SECONDARY)
        } else {
            theme::swatch(palette::ACCENT)
        };
        let tile = div()
            .relative()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::SNUG))
            .w(metrics.scale(224.0))
            .h(metrics.scale(64.0))
            .px(metrics.scale(spacing::SNUG + 2.0))
            .rounded(px(radius::LARGE + 2.0))
            .border_1()
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(metrics.scale(36.0))
                    .h(metrics.scale(36.0))
                    .rounded(px(radius::MEDIUM + 1.0))
                    .bg(theme::swatch(if custom {
                        palette::SURFACE_SELECTED
                    } else {
                        palette::ACCENT_TINT
                    }))
                    .child(icons::icon(
                        if custom { Icon::Search } else { Icon::Terminal },
                        metrics.icon_size() + px(2.0),
                        emblem_color,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(metrics.scale(2.0))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::HAIRLINE + 2.0))
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(metrics.body_size() + px(1.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .child(SharedString::from(name)),
                            )
                            .when(selected && !custom, |row| {
                                row.child(components::chip(
                                    strings.shell_default,
                                    Tone::Accent,
                                    metrics,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .when(!custom, |line| {
                                line.font_family(terminal::monospace_family())
                            })
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(
                                program.unwrap_or_else(|| strings.shell_custom_hint.to_owned()),
                            )),
                    ),
            );
        if selected {
            tile.bg(theme::swatch(palette::SURFACE_RAISED))
                .border_color(theme::faded(theme::swatch(palette::ACCENT), 0.7))
                .shadow_md()
        } else {
            tile.bg(theme::swatch(palette::SURFACE))
                .border_color(theme::swatch(palette::BORDER))
                .hover(move |style| style.bg(hover).border_color(hover_border))
        }
    }

    /// The selected terminal, the session it belongs to and the shell it runs.
    ///
    /// The terminal fills the panel edge to edge with the bar of its session on top,
    /// so the name, the state and the actions are read in the same place as the output.
    fn terminal_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .bg(theme::swatch(palette::CANVAS));

        // A shell that failed to start says so above the terminal that is still open.
        if let Some(reason) = &self.local_error {
            column = column.child(
                div()
                    .flex_none()
                    .px(metrics.scale(spacing::SNUG))
                    .pt(metrics.scale(spacing::TIGHT))
                    .child(self.notice_line(reason.clone(), Tone::Danger)),
            );
        }

        let id = self.active;
        let session = id.and_then(|id| self.workspace.session(id));
        // Why the selected session ended, and whether it can be started again.
        //
        // The session record is asked first, not the pane: a refused reconnect is
        // recorded on the session and leaves the pane holding the reason its old
        // process ended, so a notice read from the pane alone would keep stating the
        // older problem and never mention the reconnect at all.
        let ended: Option<(SessionFailure, bool)> =
            session.and_then(|session| match session.state() {
                SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
                    Some((failure.clone(), may_reconnect(session)))
                }
                _ => None,
            });

        let mut frame = div().flex().flex_col().flex_1().min_h(px(0.0));

        if let Some(pane) = self.active_pane() {
            frame = frame.child(self.session_bar(id, cx));
            // A shell that ended keeps its last screen, but the reason it stopped is
            // stated above it instead of being left for the user to guess.
            let notice = if let Some((failure, reconnectable)) = ended {
                Some(self.failure_notice(&failure, reconnectable))
            } else {
                pane.read(cx)
                    .state()
                    .failure()
                    .map(|failure| self.failure_notice(&failure, false))
            };
            if let Some(notice) = notice {
                frame = frame.child(
                    div()
                        .flex_none()
                        .px(metrics.scale(spacing::SNUG))
                        .pt(metrics.scale(spacing::SNUG))
                        .child(notice),
                );
            }
            frame = frame.child(pane.clone());
        } else if let Some(session) = session {
            // A session can outlive its terminal: a reconnect that was refused before
            // it opened one, or a process that never started. The session still holds
            // the reason, so it is drawn where the terminal would be instead of being
            // recorded and shown nowhere.
            frame = frame.child(self.session_bar(id, cx));
            let notice = if let Some((failure, reconnectable)) = ended {
                self.failure_notice(&failure, reconnectable)
            } else {
                // Nothing failed and there is nothing to draw: the reason is still
                // said, because an empty panel would look like a terminal that is
                // merely blank.
                self.notice_line(
                    format!(
                        "{} · {}",
                        strings.session_no_terminal,
                        copy::session_state_label(session.state(), self.workspace.locale())
                    ),
                    Tone::Warning,
                )
            };
            frame = frame.child(div().p(metrics.scale(spacing::SNUG)).child(notice));
        }
        column = column.child(frame);
        self.with_files_panel(column, id, cx)
    }

    /// The terminal, with the file view beside it while one is open (R9).
    fn with_files_panel(&self, column: Div, id: Option<SessionId>, cx: &mut Context<Self>) -> Div {
        let Some(id) = id.filter(|id| self.files.is_open(*id)) else {
            return column;
        };
        div()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
                    .child(column),
            )
            .child(self.files_panel(id, cx))
    }

    /// Opens or closes the file view of one session (R9).
    ///
    /// Opening asks the connection for the login directory and nothing else, so the
    /// path on screen is only ever one the server resolved.
    fn toggle_files(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.files.is_open(id) {
            self.close_files_question(id);
            self.files.close(id);
        } else if self.files.open(id) {
            self.watch_files(cx);
        }
        cx.notify();
    }

    fn files_up(&mut self, id: SessionId, cx: &mut Context<Self>) {
        // Leaving the directory ends any question about an entry in it: the question
        // names a target in the directory on screen, and a later answer would apply to
        // wherever the user went next.
        self.close_files_question(id);
        self.files.up(id);
        cx.notify();
    }

    fn files_refresh(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.files.refresh(id);
        cx.notify();
    }

    fn files_enter(&mut self, id: SessionId, name: &str, cx: &mut Context<Self>) {
        self.close_files_question(id);
        self.files.enter(id, name);
        cx.notify();
    }

    /// Drops the question of one session, if it has one.
    fn close_files_question(&mut self, id: SessionId) {
        if self
            .files_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.session() == id)
        {
            self.files_dialog = None;
            self.files_field = None;
        }
    }

    /// Ends everything the file view of one session held (R9, R11).
    ///
    /// The connection ends with the session, so the listing ends with it too: a path
    /// and its entries that nothing backs would keep showing files the user cannot
    /// reach, and a question about one of them would outlive the server it was about.
    /// The poll loop sees at its next tick that no view is left open and stops.
    fn end_files(&mut self, id: SessionId) {
        self.close_files_question(id);
        self.files.detach(id);
        // A transfer of this session has nowhere left to go: its connection is the one
        // that just ended, so its tasks are cancelled while the tasks of every other
        // session keep their state (R10).
        self.transfers.detach(id);
        self.sync_transfer_section();
    }

    // -----------------------------------------------------------------------
    // Changes to a remote directory (R11)
    // -----------------------------------------------------------------------

    /// Asks for the name of a new directory, without sending anything yet.
    fn files_new(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if !self.files.is_open(id) {
            return;
        }
        let placeholder = self.strings().files_new_placeholder;
        let field = cx.new(|cx| TextField::new(placeholder, cx));
        self.open_files_dialog(
            FilesDialog::NewDirectory {
                session: id,
                text: String::new(),
                problem: None,
            },
            field,
            cx,
        );
    }

    /// Asks what one entry should be called, with its current name already in the field.
    fn files_ask_rename(&mut self, id: SessionId, name: &str, cx: &mut Context<Self>) {
        if !self.files.is_open(id) {
            return;
        }
        let placeholder = self.strings().files_rename_placeholder;
        let field = cx.new(|cx| TextField::with_text(placeholder, name, cx));
        self.open_files_dialog(
            FilesDialog::Rename {
                session: id,
                from: name.to_owned(),
                text: name.to_owned(),
                problem: None,
            },
            field,
            cx,
        );
    }

    /// Asks whether one entry should be deleted, naming it and what it is (R11).
    ///
    /// Nothing is typed here, so no field is created and the keyboard stays where it
    /// was: the question is answered with one of two buttons, and the target is a fact
    /// from the listing rather than something the user could mistype.
    fn files_ask_delete(
        &mut self,
        id: SessionId,
        name: &str,
        kind: FileKind,
        cx: &mut Context<Self>,
    ) {
        if !self.files.is_open(id) {
            return;
        }
        self.files_dialog = Some(FilesDialog::Delete {
            session: id,
            name: name.to_owned(),
            kind,
            problem: None,
        });
        self.files_field = None;
        cx.notify();
    }

    /// Shows a question and binds its field, sending nothing.
    fn open_files_dialog(
        &mut self,
        dialog: FilesDialog,
        field: Entity<TextField>,
        cx: &mut Context<Self>,
    ) {
        self.files_dialog = Some(dialog);
        let view = cx.entity().downgrade();
        field.update(cx, |text_field, _cx| {
            text_field.on_submit(move |_text, _window, app| {
                view.update(app, |view, cx| {
                    view.submit_files_change(cx);
                    // The field keeps its text: a refused name must stay visible beside
                    // the reason it was refused.
                    false
                })
                .unwrap_or(false)
            });
        });
        cx.observe(&field, move |view, text_field, cx| {
            let text = text_field.read(cx).text().to_owned();
            if let Some(dialog) = view.files_dialog.as_mut() {
                dialog.set_text(text);
            }
            cx.notify();
        })
        .detach();
        let handle = field.read(cx).focus_handle().clone();
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_view, window, _cx| window.focus(&handle));
        }
        self.files_field = Some(field);
        cx.notify();
    }

    /// Closes the question without sending anything at all (R11).
    ///
    /// Cancelling is not a request the server has to refuse: the target never left
    /// this process, so there is nothing on the server to undo.
    fn files_cancel_change(&mut self, cx: &mut Context<Self>) {
        self.files_dialog = None;
        self.files_field = None;
        cx.notify();
    }

    /// Sends the change the question described, or says why it cannot be sent.
    ///
    /// Every answer to this is visible. A deletion the browser refuses - the entry was
    /// removed by someone else since the last Refresh, or its directory is no longer
    /// open - keeps the question on screen with the reason beside it, because a
    /// confirmation button that does nothing cannot be told apart from a broken one.
    fn submit_files_change(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.files_dialog.clone() else {
            return;
        };
        // A transfer question is answered by the transfer view, not by the browser: it
        // creates a task that the list below then reports on.
        match &dialog {
            FilesDialog::Upload { session, text, .. } => {
                self.start_upload(*session, text, cx);
                return;
            }
            FilesDialog::Download {
                session,
                remote,
                text,
                ..
            } => {
                self.start_download(*session, remote, text, cx);
                return;
            }
            FilesDialog::NewDirectory { .. }
            | FilesDialog::Rename { .. }
            | FilesDialog::Delete { .. } => {}
        }
        let sent = match &dialog {
            FilesDialog::NewDirectory { session, text, .. } => {
                self.files.create_directory(*session, text)
            }
            FilesDialog::Rename {
                session,
                from,
                text,
                ..
            } => self.files.rename(*session, from, text),
            FilesDialog::Delete { session, name, .. } => self.files.delete(*session, name),
            FilesDialog::Upload { .. } | FilesDialog::Download { .. } => Ok(()),
        };
        match sent {
            Ok(()) => {
                // The question is answered: the report and the next listing say the rest.
                self.files_dialog = None;
                self.files_field = None;
                self.watch_files(cx);
            }
            Err(problem) => {
                // A name this process can refuse on its own never reaches the server,
                // so the question stays open with the reason beside it.
                if let Some(dialog) = self.files_dialog.as_mut() {
                    dialog.fail(problem);
                }
            }
        }
        cx.notify();
    }

    /// Polls the file views for answers, until none of them is open.
    fn watch_files(&mut self, cx: &mut Context<Self>) {
        if self.files_polling {
            return;
        }
        self.files_polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FILES_POLL_INTERVAL).await;
                match this.update(cx, |view, cx| view.pump_files(cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Applies what the file views reported, returning whether to keep polling.
    ///
    /// A failure here touches the file view only: the session keeps its own state, so
    /// a server whose files cannot be read is never reported as a terminal that
    /// ended.
    fn pump_files(&mut self, cx: &mut Context<Self>) -> bool {
        let listed = self.files.pump();
        let transferred = self.transfers.pump();
        if transferred {
            // The transfers page reads one list, so it is refreshed in the same tick in
            // which the task that changed it was applied.
            self.sync_transfer_section();
        }
        if listed || transferred {
            cx.notify();
        }
        // The loop keeps running while a view is open or a transfer is still able to
        // change: a transfer outlives no view, and a finished one needs no timer.
        let open = self.files.any_open() || self.transfers.any_active();
        if !open {
            self.files_polling = false;
        }
        open
    }

    /// The file view: the host it belongs to, the path it shows and its entries.
    fn files_panel(&self, id: SessionId, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let Some(browser) = self.files.browser(id) else {
            return div().id("files.panel");
        };
        let host = browser.host().to_owned();
        let target = browser.target().to_owned();
        let path = browser.path().map(str::to_owned);
        // Whether a change is with the server decides what the panel says while the
        // answer is missing; the question itself reads its target from the browser.
        let changing = browser.is_changing();
        let state = browser.state().clone();

        let header = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "files.header".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::kind_badge(
                        strings.files_host_label,
                        true,
                        metrics,
                    ))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "files.host".to_owned())
                            .child(SharedString::from(format!("{host} · {target}"))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::kind_badge(
                        strings.files_path_label,
                        false,
                        metrics,
                    ))
                    .child(
                        // The path wraps instead of being cut off: a deep path is the
                        // one thing the user needs to be able to read here, and an
                        // ellipsis in the middle of it names no directory at all.
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .font_family(terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "files.path".to_owned())
                            .child(SharedString::from(
                                path.clone()
                                    .unwrap_or_else(|| strings.files_unknown_path.to_owned()),
                            )),
                    ),
            );

        let toolbar = div()
            .flex()
            .items_center()
            .flex_wrap()
            .flex_none()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(self.text_button(
                SharedString::from("files.up"),
                strings.files_up,
                Variant::Ghost,
                cx,
                move |view, cx| view.files_up(id, cx),
            ))
            .child(self.text_button(
                SharedString::from("files.refresh"),
                strings.files_refresh,
                Variant::Ghost,
                cx,
                move |view, cx| view.files_refresh(id, cx),
            ))
            .child(self.text_button(
                SharedString::from("files.new"),
                strings.files_new,
                Variant::Ghost,
                cx,
                move |view, cx| view.files_new(id, cx),
            ))
            // The upload asks where the file is, the download is asked for from the row
            // of the file it would copy: each question names its own object.
            .child(self.text_button(
                SharedString::from("files.upload"),
                strings.transfers_upload,
                Variant::Soft,
                cx,
                move |view, cx| view.files_ask_upload(id, cx),
            ))
            .child(self.text_button(
                SharedString::from("files.close"),
                strings.files_close,
                Variant::Ghost,
                cx,
                move |view, cx| view.toggle_files(id, cx),
            ));

        let body: gpui::AnyElement = match state {
            BrowseState::Loading => div()
                .flex_none()
                .rounded(px(radius::SMALL))
                .p(metrics.scale(spacing::TIGHT))
                .bg(theme::swatch(palette::SURFACE))
                .text_size(metrics.caption_size())
                .text_color(Tone::Warning.text())
                .debug_selector(|| "files.loading".to_owned())
                .child(SharedString::from(strings.files_loading))
                .into_any_element(),
            BrowseState::Failed(failure) => div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(metrics.scale(spacing::HAIRLINE))
                .rounded(px(radius::SMALL))
                .p(metrics.scale(spacing::TIGHT))
                .bg(theme::swatch(palette::SURFACE))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .debug_selector(|| "files.failure".to_owned())
                .child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(Tone::Danger.text())
                        .child(SharedString::from(strings.files_failed)),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "files.failure.reason".to_owned())
                        .child(SharedString::from(failure.to_string())),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "files.failure.note".to_owned())
                        .child(SharedString::from(strings.files_terminal_note)),
                )
                .into_any_element(),
            BrowseState::Ready(listing) => {
                let mut list = div()
                    .id("files.list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(metrics.scale(1.0))
                    .overflow_y_scroll()
                    .debug_selector(|| "files.list".to_owned());
                if path.as_deref().is_some_and(|path| path != "/") {
                    list = list.child(self.files_parent_row(id, cx));
                }
                if listing.is_empty() {
                    list = list.child(
                        div()
                            .flex_none()
                            .p(metrics.scale(spacing::HAIRLINE))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "files.empty".to_owned())
                            .child(SharedString::from(strings.files_empty)),
                    );
                }
                for (index, entry) in listing.entries().iter().enumerate() {
                    list = list.child(self.files_row(id, index, entry, cx));
                }
                list.into_any_element()
            }
        };

        let mut panel = div()
            .id("files.panel")
            .debug_selector(|| "files.panel".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w(metrics.scale(FILES_PANEL_WIDTH))
            .min_h(px(0.0))
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .p(metrics.scale(spacing::BASE - 2.0))
            .bg(theme::swatch(palette::SURFACE))
            .border_l_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(header)
            .child(toolbar);
        // The question while one is open, otherwise the fact that a change is with the
        // server. Both sit above the list: they are about the entries, not a part of them.
        if let Some(question) = self.files_question(id, cx) {
            panel = panel.child(question);
        } else if changing {
            panel = panel.child(
                div()
                    .flex_none()
                    .rounded(px(radius::SMALL))
                    .p(metrics.scale(spacing::HAIRLINE))
                    .bg(theme::swatch(palette::SURFACE))
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Warning.text())
                    .debug_selector(|| "files.pending".to_owned())
                    .child(SharedString::from(strings.files_change_pending)),
            );
        }
        panel = panel.child(body);
        // What the last change really did, item by item, stays under the list.
        if let Some(report) = self.files_report(id) {
            panel = panel.child(report);
        }
        // What is moving, how far it is and what it became, for this session only: a
        // transfer of another session belongs to that session's own list.
        panel = panel.child(self.transfers_panel(id, cx));
        panel
    }

    /// The transfers of one session: their state, their progress and the actions left (R10).
    ///
    /// Every line here comes from the queue, so nothing is drawn that a worker did not
    /// report. A task waiting for an overwrite decision is asked about above its own row,
    /// with the exact target and two buttons, because the answer decides whether a file
    /// that already exists is replaced.
    fn transfers_panel(&self, id: SessionId, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let tasks: Vec<Transfer> = self.transfers.for_session(id).cloned().collect();
        let active = tasks.iter().filter(|task| task.is_active()).count();

        let mut card = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(metrics.scale(1.0))
            .debug_selector(|| "transfers.panel".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .child(components::section_label(
                        strings.transfers_heading,
                        metrics,
                    ))
                    .child(
                        components::badge(
                            active,
                            if active > 0 {
                                Tone::Accent
                            } else {
                                Tone::Neutral
                            },
                            metrics,
                        )
                        .debug_selector(|| "transfers.count".to_owned()),
                    ),
            );

        if let Some((session, problem)) = &self.transfer_problem
            && *session == id
        {
            card = card.child(
                div()
                    .flex_none()
                    .p(metrics.scale(spacing::HAIRLINE))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(palette::SURFACE))
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Danger.text())
                    .debug_selector(|| "transfers.refused".to_owned())
                    .child(SharedString::from(transfer_problem_text(problem, strings))),
            );
        }

        if tasks.is_empty() {
            card = card.child(
                div()
                    .flex_none()
                    .p(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "transfers.empty".to_owned())
                    .child(SharedString::from(strings.transfers_empty)),
            );
            return card;
        }

        for (index, task) in tasks.iter().enumerate() {
            if task.state().is_awaiting_overwrite() {
                card = card.child(self.transfer_overwrite_question(index, task, cx));
            }
            card = card.child(self.session_transfer_row(index, task, cx));
        }
        card
    }

    /// The question about a target that already exists (R10).
    ///
    /// Nothing has been written into the target while this is shown, so both answers are
    /// safe: replacing writes the file, keeping it leaves the original exactly as it was.
    fn transfer_overwrite_question(
        &self,
        index: usize,
        task: &Transfer,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let id = task.id();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(metrics.scale(spacing::HAIRLINE))
            .pl(metrics.scale(spacing::TIGHT))
            .pr(metrics.scale(spacing::HAIRLINE))
            .py(metrics.scale(spacing::HAIRLINE + 2.0))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::DANGER_TINT))
            .border_l(metrics.scale(3.0))
            .border_color(theme::swatch(palette::DANGER))
            .text_size(metrics.caption_size())
            .debug_selector(move || format!("transfers.overwrite.{index}"))
            .child(
                div()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(move || format!("transfers.overwrite.text.{index}"))
                    .child(SharedString::from(format!(
                        "{}: {}",
                        strings.transfers_overwrite_question,
                        task.destination()
                    ))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .child(self.text_button(
                        SharedString::from(format!("transfers.overwrite.replace.{index}")),
                        strings.transfers_overwrite_replace,
                        Variant::Danger,
                        cx,
                        move |view, cx| {
                            view.transfer_decide(id, shellune_core::OverwriteChoice::Replace, cx);
                        },
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("transfers.overwrite.keep.{index}")),
                        strings.transfers_overwrite_keep,
                        Variant::Secondary,
                        cx,
                        move |view, cx| {
                            view.transfer_decide(
                                id,
                                shellune_core::OverwriteChoice::KeepExisting,
                                cx,
                            );
                        },
                    )),
            )
    }

    /// One transfer's detail line: what it is doing, why it stopped and what it left.
    ///
    /// The session's own list and the transfers page draw the same line. They have to:
    /// a task whose session ended is closed off the file view, so the page is the only
    /// place it can still be read - and a partial file it left behind is exactly what
    /// the user has to be told there.
    fn transfer_detail(&self, task: &Transfer) -> String {
        let strings = self.strings();
        let state_label = copy::transfer_state_label(task.state(), self.workspace.locale());
        let mut detail = match task.state() {
            shellune_core::TransferState::Running {
                transferred,
                total: Some(total),
            } => {
                // The share is printed only when the whole is known: a percentage
                // beside no total would be a guess about how much is left.
                let percent = task.state().permille().map_or(0, |permille| permille / 10);
                format!("{state_label} · {percent}% · {transferred}/{total} B")
            }
            shellune_core::TransferState::Running { transferred, .. } => {
                format!("{state_label} · {transferred} B")
            }
            shellune_core::TransferState::Failed { failure, .. } => {
                format!("{state_label} · {failure}")
            }
            _ => state_label.to_owned(),
        };
        if let Some(residue) = task.state().residue() {
            detail.push_str(&format!(" · {}: {residue}", strings.transfers_residue));
        }
        detail
    }

    /// One transfer of a session: what it moves, how far it is and what it became.
    fn session_transfer_row(
        &self,
        index: usize,
        task: &Transfer,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let tone = Tone::for_transfer_state(task.state());
        let id = task.id();
        let detail = self.transfer_detail(task);
        let mut actions = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::HAIRLINE));
        if task.is_active() && !task.state().is_awaiting_overwrite() {
            actions = actions.child(self.text_button(
                SharedString::from(format!("transfers.cancel.{index}")),
                strings.transfers_cancel,
                Variant::Ghost,
                cx,
                move |view, cx| view.transfer_cancel(id, cx),
            ));
        }
        if matches!(
            task.state(),
            shellune_core::TransferState::Failed { .. }
                | shellune_core::TransferState::Cancelled { .. }
        ) {
            actions = actions.child(self.text_button(
                SharedString::from(format!("transfers.retry.{index}")),
                strings.transfers_retry,
                Variant::Secondary,
                cx,
                move |view, cx| view.transfer_retry(id, cx),
            ));
        }
        div()
            .id(("transfers.row", index))
            .debug_selector(move || format!("transfers.row.{index}"))
            .flex()
            .flex_col()
            .flex_none()
            .gap(metrics.scale(2.0))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE + 2.0))
            .text_size(metrics.caption_size())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .truncate()
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(move || format!("transfers.name.{index}"))
                            .child(SharedString::from(format!(
                                "{} {} → {}",
                                theme::transfer_direction_label(task.direction()),
                                task.source(),
                                task.destination()
                            ))),
                    ),
            )
            .child(
                div()
                    .text_color(tone.text())
                    .debug_selector(move || format!("transfers.state.{index}"))
                    .child(SharedString::from(detail)),
            )
            .child(actions)
    }

    /// The question about a change, when one is being asked for this session (R11).
    ///
    /// Every variant names the exact object: the full path, and for a deletion also the
    /// kind and whether its contents go with it. Nothing on it sends anything until the
    /// confirming button is pressed.
    fn files_question(&self, id: SessionId, cx: &mut Context<Self>) -> Option<Div> {
        let dialog = self
            .files_dialog
            .as_ref()
            .filter(|dialog| dialog.session() == id)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let directory = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned);
        let danger = matches!(dialog, FilesDialog::Delete { .. });
        let mut card =
            components::banner(if danger { Tone::Danger } else { Tone::Accent }, metrics)
                .gap(metrics.scale(spacing::TIGHT - 2.0))
                .debug_selector(|| "files.question".to_owned());

        let heading = match dialog {
            FilesDialog::NewDirectory { .. } => strings.files_new,
            FilesDialog::Rename { .. } => strings.files_rename,
            FilesDialog::Delete { .. } => strings.files_delete,
            FilesDialog::Upload { .. } => strings.transfers_upload,
            FilesDialog::Download { .. } => strings.transfers_download,
        };
        card = card.child(
            div()
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .debug_selector(|| "files.question.heading".to_owned())
                .child(SharedString::from(heading)),
        );

        match dialog {
            FilesDialog::NewDirectory { .. }
            | FilesDialog::Rename { .. }
            | FilesDialog::Upload { .. }
            | FilesDialog::Download { .. } => {
                // Each question names the object it would act on: a new folder goes into
                // the directory on screen, a rename keeps the entry's full path, an upload
                // names the directory its file would land in, and a download names the
                // remote entry it would copy.
                let target = match dialog {
                    FilesDialog::Rename { from, .. } => directory
                        .as_deref()
                        .map_or_else(|| from.clone(), |directory| join_path(directory, from)),
                    FilesDialog::Upload {
                        directory,
                        text: typed,
                        ..
                    } => match file_name(typed) {
                        Some(name) => join_path(directory, name),
                        None => directory.clone(),
                    },
                    FilesDialog::Download { remote, .. } => remote.clone(),
                    _ => directory.clone().unwrap_or_default(),
                };
                card = card.child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "files.question.target".to_owned())
                        .child(SharedString::from(target)),
                );
                if let Some(field) = &self.files_field {
                    card = card.child(
                        div()
                            .flex_none()
                            .debug_selector(|| "files.question.input".to_owned())
                            .child(field.clone()),
                    );
                }
                if let Some(problem) = dialog.problem() {
                    card = card.child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(Tone::Danger.text())
                            .debug_selector(|| "files.question.problem".to_owned())
                            .child(SharedString::from(problem_text(problem, strings))),
                    );
                }
                if let Some(problem) = dialog.path_problem() {
                    card = card.child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(Tone::Danger.text())
                            .debug_selector(|| "files.question.problem".to_owned())
                            .child(SharedString::from(path_problem_text(problem, strings))),
                    );
                }
                let confirm = match dialog {
                    FilesDialog::Rename { .. } => strings.files_rename_confirm,
                    FilesDialog::Upload { .. } => strings.transfers_upload_confirm,
                    FilesDialog::Download { .. } => strings.transfers_download_confirm,
                    _ => strings.files_create,
                };
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .child(self.text_button(
                            SharedString::from("files.question.confirm"),
                            confirm,
                            Variant::Primary,
                            cx,
                            |view, cx| view.submit_files_change(cx),
                        ))
                        .child(self.text_button(
                            SharedString::from("files.question.cancel"),
                            strings.files_cancel,
                            Variant::Secondary,
                            cx,
                            |view, cx| view.files_cancel_change(cx),
                        )),
                );
            }
            FilesDialog::Delete { name, kind, .. } => {
                let target = directory
                    .as_deref()
                    .map(|directory| join_path(directory, name))
                    .unwrap_or_else(|| name.clone());
                card = card.child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .debug_selector(|| "files.question.target".to_owned())
                        .child(SharedString::from(format!(
                            "{}: {target}",
                            strings.files_target_label
                        ))),
                );
                card = card.child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "files.question.kind".to_owned())
                        .child(SharedString::from(format!(
                            "{}: {}",
                            strings.files_kind_label,
                            kind_label(*kind, strings)
                        ))),
                );
                card = card.child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "files.question.note".to_owned())
                        .child(SharedString::from(if kind.is_directory() {
                            strings.files_delete_recursive
                        } else {
                            strings.files_delete_file
                        })),
                );
                // A deletion the browser refuses says so here. Without it, confirming a
                // delete of an entry that is no longer in the directory - removed by
                // someone else, or gone since the last Refresh - would look like a button
                // that does nothing at all.
                if let Some(problem) = dialog.problem() {
                    card = card.child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(Tone::Danger.text())
                            .debug_selector(|| "files.question.problem".to_owned())
                            .child(SharedString::from(problem_text(problem, strings))),
                    );
                }
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .child(self.text_button(
                            SharedString::from("files.question.confirm"),
                            strings.files_delete_confirm,
                            Variant::Danger,
                            cx,
                            |view, cx| view.submit_files_change(cx),
                        ))
                        .child(self.text_button(
                            SharedString::from("files.question.cancel"),
                            strings.files_cancel,
                            Variant::Secondary,
                            cx,
                            |view, cx| view.files_cancel_change(cx),
                        )),
                );
            }
        }
        Some(card)
    }

    /// What the last change really did, item by item (R11).
    ///
    /// A change that failed in part is not a success with a footnote: the heading says
    /// so, and every path the server refused is listed with the reason it gave.
    fn files_report(&self, id: SessionId) -> Option<Div> {
        let outcome = self.files.outcome(id)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let done = outcome.succeeded();
        let mut card =
            components::banner(if done { Tone::Positive } else { Tone::Danger }, metrics)
                .gap(metrics.scale(1.0))
                .debug_selector(|| "files.report".to_owned())
                .child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(if done {
                            Tone::Positive.text()
                        } else {
                            Tone::Danger.text()
                        })
                        .debug_selector(|| "files.report.heading".to_owned())
                        .child(SharedString::from(if done {
                            strings.files_change_done
                        } else {
                            strings.files_change_failed
                        })),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "files.report.count".to_owned())
                        .child(SharedString::from(format!(
                            "{} {}/{}",
                            strings.files_change_changed,
                            outcome.changed(),
                            outcome.steps().len()
                        ))),
                );
        for (index, step) in outcome.steps().iter().take(FILES_REPORT_STEPS).enumerate() {
            let selector = format!("files.report.step.{index}");
            let line = match step.failure() {
                Some(failure) => format!("{} · {failure}", step.path()),
                None => format!(
                    "{} · {}",
                    step.path(),
                    step.kind()
                        .map_or(strings.files_change_changed, |kind| kind_label(
                            kind, strings
                        ))
                ),
            };
            card = card.child(
                div()
                    .min_w(px(0.0))
                    .text_size(metrics.caption_size())
                    .text_color(if step.failure().is_some() {
                        Tone::Danger.text()
                    } else {
                        theme::swatch(palette::TEXT_SECONDARY)
                    })
                    .debug_selector(move || selector.clone())
                    .child(SharedString::from(line)),
            );
        }
        Some(card)
    }

    /// The row that leads to the parent directory.
    fn files_parent_row(&self, id: SessionId, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        div()
            .id("files.parent")
            .debug_selector(|| "files.parent".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .px(metrics.scale(spacing::TIGHT))
            .h(metrics.compact_control_height())
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .cursor_pointer()
            .on_click(cx.listener(move |view, _event, _window, cx| {
                view.files_up(id, cx);
            }))
            .child(SharedString::from(format!(
                "..  {}",
                self.strings().files_parent_row
            )))
    }

    /// One entry of the listing: what it is, and the two changes it allows (R11).
    ///
    /// The row is two lines because the entry keeps its own click - opening a
    /// directory is not a change - while rename and delete are about the entry rather
    /// than about opening it. Each button names its target when it is pressed, so a
    /// narrow panel never leaves it unclear which row a question is about.
    fn files_row(
        &self,
        id: SessionId,
        index: usize,
        entry: &shellune_core::DirEntry,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let label = kind_label(entry.kind(), strings);
        let name = entry.name();
        let size = entry.size();
        let kind = entry.kind();
        let openable = entry.is_directory();
        let owned = name.to_owned();
        let rename = name.to_owned();
        let delete = name.to_owned();
        let download = name.to_owned();
        let name_selector = format!("files.name.{index}");
        let mut entry_row = div()
            .id(("files.entry", index))
            .debug_selector(move || format!("files.entry.{index}"))
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(2.0))
            .rounded(px(radius::SMALL))
            .text_size(metrics.label_size())
            .child(
                div()
                    .min_w(px(0.0))
                    .truncate()
                    .when(openable, |name| name.font_weight(FontWeight::MEDIUM))
                    .text_color(theme::swatch(if openable {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_SECONDARY
                    }))
                    .debug_selector(move || name_selector.clone())
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(match size {
                        Some(bytes) => format!("{label} · {bytes} B"),
                        None => label.to_owned(),
                    })),
            );
        if openable {
            entry_row = entry_row.cursor_pointer().on_click(cx.listener(
                move |view, _event, _window, cx| {
                    view.files_enter(id, &owned, cx);
                },
            ));
        }
        let hover = theme::swatch(palette::SURFACE_HOVER);
        div()
            .id(("files.row", index))
            .debug_selector(move || format!("files.row.{index}"))
            .flex()
            .flex_col()
            .flex_none()
            .py(metrics.scale(2.0))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .hover(move |style| style.bg(hover))
            .child(entry_row)
            .child({
                let mut actions = div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .px(metrics.scale(spacing::HAIRLINE))
                    .pb(metrics.scale(1.0))
                    .child(self.text_button(
                        SharedString::from(format!("files.rename.{index}")),
                        strings.files_rename,
                        Variant::Ghost,
                        cx,
                        move |view, cx| view.files_ask_rename(id, &rename, cx),
                    ));
                // Only a file has content to copy, so only a file offers a download: a
                // directory that showed the button would promise a transfer it cannot do.
                if !openable {
                    actions = actions.child(self.text_button(
                        SharedString::from(format!("files.download.{index}")),
                        strings.transfers_download,
                        Variant::Ghost,
                        cx,
                        move |view, cx| view.files_ask_download(id, &download, cx),
                    ));
                }
                actions.child(self.text_button(
                    SharedString::from(format!("files.delete.{index}")),
                    strings.files_delete,
                    Variant::Ghost,
                    cx,
                    move |view, cx| view.files_ask_delete(id, &delete, kind, cx),
                ))
            })
    }
    /// tests that check what the window does with a listing therefore attach their
    /// own channel here.
    #[cfg(test)]
    pub(crate) fn attach_files_for_test(
        &mut self,
        id: SessionId,
        host: &str,
        target: &str,
        channel: Box<dyn BrowseChannel>,
        cx: &mut Context<Self>,
    ) {
        self.files.attach(id, host, target, channel);
        cx.notify();
    }

    /// Attaches a scripted transfer channel to one session, for a test.
    ///
    /// The window creates the real one when a connection becomes live; a test that
    /// checks what the window does with a running transfer therefore supplies its own.
    #[cfg(test)]
    pub(crate) fn attach_transfers_for_test(
        &mut self,
        id: SessionId,
        channel: Box<dyn TransferChannel>,
        cx: &mut Context<Self>,
    ) {
        self.transfers.attach(id, channel);
        cx.notify();
    }

    /// The session whose close is being asked about, for a test.
    #[cfg(test)]
    pub fn closing_for_test(&self) -> Option<SessionId> {
        self.closing
    }

    /// Replaces the text of the open question, for a test that types without a keyboard.
    #[cfg(test)]
    pub fn files_question_set_text_for_test(&mut self, text: &str) {
        if let Some(dialog) = self.files_dialog.as_mut() {
            dialog.set_text(text.to_owned());
        }
    }

    /// Sends the open question, for a test that cannot press the confirm button.
    #[cfg(test)]
    pub(crate) fn submit_files_change_for_test(&mut self, cx: &mut Context<Self>) {
        self.submit_files_change(cx);
    }

    /// Every transfer the window knows about, for a test.
    #[cfg(test)]
    pub fn transfer_tasks(&self) -> Vec<Transfer> {
        self.transfers.transfers().to_vec()
    }

    /// How many tasks of one session can still change, for a test.
    #[cfg(test)]
    pub fn transfer_active_count(&self, id: SessionId) -> usize {
        self.transfers.active_count(id)
    }

    /// What a transfer's row says, exactly as the panel and the page draw it.
    #[cfg(test)]
    pub fn transfer_detail_for_test(&self, id: shellune_core::TransferId) -> Option<String> {
        self.transfers
            .get(id)
            .map(|task| self.transfer_detail(task))
    }

    /// The file view of one session, as the window holds it.
    #[cfg(test)]
    pub fn files_browser(&self, id: SessionId) -> Option<&shellune_core::FileBrowser> {
        self.files.browser(id)
    }

    /// Whether the file view of a session is open.
    #[cfg(test)]
    pub fn files_visible(&self, id: SessionId) -> bool {
        self.files.is_open(id)
    }

    /// Whether a session's connection can still be browsed, for a test.
    #[cfg(test)]
    pub fn files_can_browse(&self, id: SessionId) -> bool {
        self.files.can_browse(id)
    }

    /// Applies what the file views reported, for a test that cannot wait for a timer.
    #[cfg(test)]
    pub(crate) fn pump_files_for_test(&mut self, cx: &mut Context<Self>) -> bool {
        self.pump_files(cx)
    }

    /// Whether the file view is asking about a change, for a test.
    #[cfg(test)]
    pub fn files_question_open(&self) -> bool {
        self.files_dialog.is_some()
    }

    /// The text the open change question carries, for a test.
    #[cfg(test)]
    pub fn files_question_text(&self) -> Option<&str> {
        self.files_dialog.as_ref().and_then(FilesDialog::text)
    }

    /// Why a session ended, and what the user can do about it (R12).
    ///
    /// The reason and the next step are one notice because the requirement is about
    /// both: an error the user cannot act on is only half reported. The step is the
    /// action the session's own state allows - reconnect while it may be restarted,
    /// close once it may not.
    fn failure_notice(&self, failure: &SessionFailure, reconnectable: bool) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let step = if reconnectable {
            strings.session_reconnect
        } else {
            strings.session_close
        };
        // A process that ended with status 0 is stated in the neutral tone: the reason
        // is still shown, but a normal exit is not an error.
        let tone = Tone::for_failure(failure);
        components::banner(tone, metrics)
            .debug_selector(|| "terminal.failure".to_owned())
            .child(
                div()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(failure.to_string())),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "terminal.failure.next".to_owned())
                    .child(SharedString::from(format!(
                        "{} · {step}",
                        strings.next_step
                    ))),
            )
    }

    /// One coloured notice line under a heading.
    fn notice_line(&self, text: String, tone: Tone) -> Div {
        let metrics = self.metrics();
        components::banner(tone, metrics)
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .debug_selector(|| "session.notice".to_owned())
            .child(SharedString::from(text))
    }

    /// The tab of one session: what it is, what it runs and what it is doing.
    ///
    /// Every part R7 asks for is here: the name, the kind and the connection state are
    /// read from the session record, so a tab cannot describe a session the domain
    /// model does not have. The state is a coloured dot while the session is live and
    /// is spelled out once it is anything else; the object it talks to is on the
    /// session bar and in the tab's tooltip.
    fn session_tab(&self, id: SessionId, index: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let Some(session) = self.workspace.session(id) else {
            return div().id(("session.tab", index));
        };
        let selected = self.active == Some(id);
        let kind = PaneKind::of(session.kind());
        let tone = session_tone(session.state());
        let state_label = copy::session_state_label(session.state(), self.workspace.locale());
        let live = session.state() == &SessionState::Connected;
        let kind_label = match kind {
            PaneKind::Local => strings.session_kind_local,
            PaneKind::Remote => strings.session_kind_remote,
        };
        let jump = if index < crate::app::TAB_JUMP_COUNT {
            format!(" ({})", Platform::current().tab_jump_label(index + 1))
        } else {
            String::new()
        };
        let tooltip = format!(
            "{} · {kind_label} · {} · {state_label}{jump}",
            session.title(),
            session_target(&self.book, session)
        );
        let selector = format!("session.tab.{index}");
        let hover = theme::swatch(palette::SURFACE_HOVER);
        let hover_text = theme::swatch(palette::TEXT_PRIMARY);

        let mut tab = div()
            .id(("session.tab", index))
            .debug_selector(move || selector.clone())
            .relative()
            .flex()
            .items_center()
            .flex_shrink()
            .min_w(metrics.scale(84.0))
            .max_w(metrics.scale(220.0))
            .gap(metrics.scale(spacing::TIGHT - 1.0))
            .h(metrics.tab_height())
            .pl(metrics.scale(spacing::SNUG - 2.0))
            .pr(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .text_size(metrics.label_size())
            .cursor_pointer()
            .tooltip(components::tooltip(tooltip))
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.activate_in(id, window, cx);
            }))
            // A middle click closes a tab and a right click offers the other tab
            // actions, as in a browser or any other tabbed terminal.
            .on_mouse_down(
                gpui::MouseButton::Middle,
                cx.listener(move |this, _event, _window, cx| {
                    cx.stop_propagation();
                    this.request_close_session(id, cx);
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_tab_menu(id, event, window, cx);
                }),
            );
        tab = if selected {
            tab.bg(theme::swatch(palette::CANVAS))
                .border_color(theme::swatch(palette::BORDER_STRONG))
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .shadow_sm()
        } else {
            tab.border_color(theme::clear())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .hover(move |style| style.bg(hover).text_color(hover_text))
        };

        tab.child(components::status_dot(tone, metrics))
            .when(kind == PaneKind::Remote, |tab| {
                tab.child(components::kind_badge(kind_label, true, metrics))
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .when(selected, |label| label.font_weight(FontWeight::MEDIUM))
                    .child(SharedString::from(self.tab_title(id, index))),
            )
            .when(!live, |tab| {
                tab.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(tone.text())
                        .child(SharedString::from(state_label)),
                )
            })
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    .w(metrics.scale(20.0))
                    .h(metrics.scale(20.0))
                    .id(("session.tab.close", index))
                    .debug_selector({
                        let selector = format!("session.tab.close.{index}");
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.stop_propagation();
                        this.request_close_session(id, cx);
                    })),
            )
    }

    /// The label of one tab: the session's title, numbered when an earlier tab has
    /// the same one, so two shells of one program can be told apart at a glance.
    fn tab_title(&self, id: SessionId, index: usize) -> String {
        let Some(title) = self.workspace.session(id).map(Session::title) else {
            return String::new();
        };
        let tabs = self.tab_sessions();
        let same = |other: &SessionId| {
            self.workspace
                .session(*other)
                .is_some_and(|session| session.title() == title)
        };
        let before = tabs.iter().take(index).filter(|other| same(other)).count();
        let total = tabs.iter().filter(|other| same(other)).count();
        if total > 1 {
            format!("{title} {}", before + 1)
        } else {
            title.to_owned()
        }
    }

    /// The tabs of the sessions that are still open, in the order they were opened.
    fn tab_row(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let mut row = div()
            .flex()
            .items_center()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .gap(metrics.scale(spacing::HAIRLINE))
            .debug_selector(|| "session.tabs".to_owned());
        for (index, id) in self.tab_sessions().iter().enumerate() {
            row = row.child(self.session_tab(*id, index, cx));
        }
        row
    }

    /// The tab strip a list page shows while sessions are open, with the button that
    /// opens another local shell; on a terminal page the tabs live in the header.
    fn tab_strip(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(metrics.scale(spacing::HAIRLINE))
            .h(metrics.scale(44.0))
            .px(metrics.scale(spacing::TIGHT))
            .bg(theme::swatch(palette::SURFACE))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(self.tab_row(cx).flex_initial())
            .child(
                components::icon_only_button(Icon::Plus, metrics)
                    .id("session.tab.new")
                    .debug_selector(|| "session.tab.new".to_owned())
                    .tooltip(components::tooltip(SharedString::from(format!(
                        "{} ({})",
                        strings.primary_local,
                        new_shell_shortcut_label(Platform::current())
                    ))))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.new_local_shell(window, cx);
                    })),
            )
    }

    /// The state, the object and the actions of the selected session.
    fn session_bar(&self, id: Option<SessionId>, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let Some(session) = id.and_then(|id| self.workspace.session(id)) else {
            return div();
        };
        let id = session.id();
        let kind = PaneKind::of(session.kind());
        let kind_label = match kind {
            PaneKind::Local => strings.session_kind_local,
            PaneKind::Remote => strings.session_kind_remote,
        };
        let target = match session.kind() {
            SessionKind::Local { .. } => program_label(&session_target(&self.book, session)),
            SessionKind::Remote { .. } => session_target(&self.book, session),
        };
        // A local shell's program is often its own title; saying it twice is noise.
        let target = (target.to_lowercase() != session.title().to_lowercase()).then_some(target);
        let state_label = copy::session_state_label(session.state(), self.workspace.locale());
        let tone = session_tone(session.state());
        let can_reconnect = may_reconnect(session);
        // Only a connection that can really browse offers the file view: a local
        // session has no server to ask, and a session whose connection ended has no
        // directory left to show.
        let browsable = self.files.can_browse(id);
        let files_open = self.files.is_open(id);
        let files_button = browsable.then(|| {
            self.icon_text_button(
                SharedString::from("session.files"),
                Icon::Folder,
                if files_open {
                    strings.files_close
                } else {
                    strings.files_open
                },
                if files_open {
                    Variant::Secondary
                } else {
                    Variant::Soft
                },
                cx,
                move |view, cx| view.toggle_files(id, cx),
            )
        });

        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .gap(metrics.scale(spacing::TIGHT))
            .h(metrics.session_bar_height())
            .pl(metrics.scale(spacing::BASE - 2.0))
            .pr(metrics.scale(spacing::TIGHT))
            .bg(theme::swatch(palette::CANVAS))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "session.bar".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::kind_badge(
                        kind_label,
                        kind == PaneKind::Remote,
                        metrics,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .max_w(metrics.scale(240.0))
                            .truncate()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "session.identity".to_owned())
                            .child(SharedString::from(format!(
                                "{} · {}",
                                strings.workspace_current,
                                session.title()
                            ))),
                    )
                    .children(target.map(|target| {
                        div()
                            .min_w(px(0.0))
                            .truncate()
                            .font_family(terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(target))
                    }))
                    .child(components::chip(state_label, tone, metrics))
                    .children(
                        self.unsaved
                            .iter()
                            .find(|(session, _)| *session == id)
                            .map(|_| {
                                div()
                                    .flex_none()
                                    .truncate()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED))
                                    .child(strings.quick_unsaved_note)
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .when_some(files_button, |row, button| row.child(button))
                    .when(can_reconnect, |row| {
                        row.child(self.icon_text_button(
                            SharedString::from("session.reconnect"),
                            Icon::Refresh,
                            strings.session_reconnect,
                            Variant::Primary,
                            cx,
                            move |view, cx| view.reconnect_session(id, cx),
                        ))
                    })
                    .when(
                        self.unsaved.iter().any(|(session, _)| *session == id),
                        |row| {
                            row.child(self.icon_text_button(
                                SharedString::from("session.save_host"),
                                Icon::Server,
                                strings.quick_save_host,
                                Variant::Primary,
                                cx,
                                move |view, cx| view.save_quick_host(id, cx),
                            ))
                        },
                    )
                    .child(self.icon_text_button(
                        SharedString::from("session.close"),
                        Icon::Close,
                        strings.session_close,
                        Variant::Ghost,
                        cx,
                        move |view, cx| view.request_close_session(id, cx),
                    )),
            )
    }

    fn local_rows(&self, list: Div, cx: &mut Context<Self>) -> Div {
        let sessions: Vec<&Session> = self.workspace.local_sessions().collect();
        self.session_rows_from(list, &sessions, cx)
    }

    fn session_rows(&self, list: Div, cx: &mut Context<Self>) -> Div {
        let sessions: Vec<&Session> = self.workspace.sessions().items().iter().collect();
        self.session_rows_from(list, &sessions, cx)
    }

    fn session_rows_from(
        &self,
        mut list: Div,
        sessions: &[&Session],
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(bar) = self.closed_sessions_bar(sessions, cx) {
            list = list.child(bar);
        }
        for (index, session) in sessions.iter().enumerate() {
            let id = session.id();
            // A row whose session still has a terminal opens it; one that has none
            // only reports, because there is nothing to show.
            let openable = self.pane_for(id).is_some();
            list = list.child(
                div()
                    .id(("row", index))
                    .debug_selector(move || format!("row.{index}"))
                    .when(openable, |row| {
                        row.cursor_pointer().on_click(cx.listener(
                            move |this, _event, window, cx| {
                                this.activate_in(id, window, cx);
                            },
                        ))
                    })
                    // Every row, open or closed, offers its actions on a right click.
                    .on_mouse_down(
                        sessions::MENU_BUTTON,
                        self.session_row_menu_listener(id, cx),
                    )
                    .child(self.session_row(session, self.session_row_actions(id, index, cx))),
            );
        }
        list
    }

    fn transfer_rows(&self, mut list: Div) -> Div {
        for (index, transfer) in self.workspace.transfers().items().iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.transfer_row(transfer)),
            );
        }
        list
    }

    /// One session in a list: what it is on the left, its state and `actions` on the
    /// right.
    fn session_row(&self, session: &Session, actions: Div) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let tone = Tone::for_session_state(session.state());
        let kind = theme::session_kind_label(session.kind(), strings.local_shell_fallback);
        let remote = PaneKind::of(session.kind()) == PaneKind::Remote;
        // A failed or lost session states why it ended and, next to it, the action its
        // own state allows (R12): a reason the user cannot act on is only half a
        // report, and this row is the one place a session without a terminal is still
        // shown.
        let detail = match session.state() {
            SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
                let step = if may_reconnect(session) {
                    strings.session_reconnect
                } else {
                    strings.session_close
                };
                format!(
                    "{} · {} · {step}",
                    theme::session_failure_text(failure),
                    strings.next_step
                )
            }
            _ => session_target(&self.book, session),
        };
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(components::kind_badge(
                        if remote {
                            strings.session_kind_remote.to_owned()
                        } else {
                            kind
                        },
                        remote,
                        metrics,
                    ))
                    .child(components::stacked(
                        session.title().to_owned(),
                        detail,
                        metrics,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(components::chip(
                        copy::session_state_label(session.state(), locale),
                        tone,
                        metrics,
                    ))
                    .child(actions),
            )
    }

    fn transfer_row(&self, transfer: &Transfer) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let tone = Tone::for_transfer_state(transfer.state());
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(components::kind_badge(
                        theme::transfer_direction_label(transfer.direction()),
                        false,
                        metrics,
                    ))
                    .child(components::stacked(
                        transfer.source().to_owned(),
                        // The same line the session panel draws, residue included: a
                        // task whose session is gone can only be read here, and a
                        // partial file it left is what the user has to act on.
                        format!(
                            "\u{2192} {} · {}",
                            transfer.destination(),
                            self.transfer_detail(transfer)
                        ),
                        metrics,
                    )),
            )
            .child(components::chip(
                copy::transfer_state_label(transfer.state(), locale),
                tone,
                metrics,
            ))
    }

    /// Switches the interface language, and tells every open terminal about it.
    ///
    /// Two paths reach this: the shortcut, which only applies outside a terminal, and
    /// the language control in the status bar, which is how the language is switched
    /// while a terminal has the keyboard (there Ctrl+L belongs to the shell).
    fn toggle_locale(&mut self, cx: &mut Context<Self>) {
        self.workspace.toggle_locale();
        let locale = self.workspace.locale();
        // Every open terminal keeps its own labels, so each one is told: the ones
        // that are not on screen must come back in the new language too.
        let panes: Vec<Entity<TerminalPane>> =
            self.panes.iter().map(|open| open.pane.clone()).collect();
        for pane in panes {
            pane.update(cx, |pane, cx| pane.set_locale(locale, cx));
        }
        cx.notify();
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

impl Render for ShelluneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Small views that are not handed the workspace (a text field's menu) read the
        // language from here.
        self.workspace.locale().set_current();
        // The palette is decided before anything is drawn: the user's choice, or the
        // system's when the choice is to follow it, which the window reports when it
        // changes.
        if !self.watching_appearance {
            self.watching_appearance = true;
            cx.observe_window_appearance(window, |this, _window, cx| {
                this.repaint_terminals(cx);
                cx.notify();
            })
            .detach();
        }
        let system_dark = matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        theme::set_appearance(self.theme_choice.appearance(system_dark));
        // A panel that closed while one of its fields had the keyboard leaves nothing
        // focused, and every shortcut would then go nowhere; the page takes it back.
        if window.focused(cx).is_none() {
            let destination = self.workspace.destination();
            let handle = self.destination_focus(destination).clone();
            window.focus(&handle);
        }
        if std::mem::take(&mut self.focus_host_dialog)
            && let Some(fields) = &self.host_fields
        {
            let handle = fields.name.read(cx).focus_handle().clone();
            window.focus(&handle);
        }
        let metrics = self.metrics();
        let notice = self.notice_strip(cx);

        div()
            .id("shellune.root")
            .flex()
            .size_full()
            .bg(theme::swatch(palette::CANVAS))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .key_context("Shellune")
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &GoToLocalShells, window, cx| {
                this.select(Destination::LocalShells, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToHosts, window, cx| {
                this.select(Destination::Hosts, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToSessions, window, cx| {
                this.select(Destination::Sessions, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToTransfers, window, cx| {
                this.select(Destination::Transfers, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextDestination, window, cx| {
                this.workspace.select_next();
                let next = this.workspace.destination();
                this.select(next, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousDestination, window, cx| {
                this.workspace.select_previous();
                let previous = this.workspace.destination();
                this.select(previous, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CycleTextSize, _window, cx| {
                this.workspace.cycle_text_size();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleLanguage, _window, cx| {
                this.toggle_locale(cx);
            }))
            .on_action(cx.listener(|this, _: &DismissNotice, window, cx| {
                // Escape closes whatever floats first: the palette, a menu, then the
                // notice.
                if this.palette.is_some() {
                    this.close_palette(window, cx);
                    return;
                }
                if this.host_fields.is_some() {
                    this.close_host_editor(cx);
                    return;
                }
                if this.tab_menu.is_some() {
                    this.close_tab_menu(window, cx);
                    return;
                }
                if this.session_menu.is_some() {
                    this.close_session_menu(window, cx);
                    return;
                }
                if this.shell_menu_open {
                    this.close_shell_menu(cx);
                    return;
                }
                this.workspace.dismiss_notice();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NewLocalShell, window, cx| {
                this.new_local_shell(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.toggle_rail(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| {
                this.toggle_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseActiveTab, window, cx| {
                if let Some(id) = this.active {
                    this.request_close_session(id, cx);
                    // The tab that takes over gets the keyboard through this window;
                    // the pane's own focus call cannot reach it while the action runs,
                    // and the next shortcut would otherwise go nowhere.
                    this.restore_focus(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.cycle_tab(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.cycle_tab(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab1, window, cx| {
                this.select_tab(0, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab2, window, cx| {
                this.select_tab(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab3, window, cx| {
                this.select_tab(2, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab4, window, cx| {
                this.select_tab(3, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab5, window, cx| {
                this.select_tab(4, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab6, window, cx| {
                this.select_tab(5, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab7, window, cx| {
                this.select_tab(6, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab8, window, cx| {
                this.select_tab(7, window, cx);
            }))
            // The last jump goes to the last tab, however many there are.
            .on_action(cx.listener(|this, _: &SelectTab9, window, cx| {
                let last = this.tab_sessions().len().saturating_sub(1);
                this.select_tab(last, window, cx);
            }))
            .children(self.palette_overlay(window, cx))
            .children(self.tab_menu_overlay(window, cx))
            .children(self.session_menu_overlay(window, cx))
            .children(self.host_dialog_overlay(window, cx))
            .child(self.rail(window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(self.header(window, cx))
                    .children(notice)
                    .children(
                        (!self.tab_sessions().is_empty()
                            && !self.shows_terminal(self.workspace.destination()))
                        .then(|| self.tab_strip(cx)),
                    )
                    .children(
                        (!self.shows_terminal(self.workspace.destination())
                            && self.active.is_some())
                        .then(|| self.session_bar(self.active, cx)),
                    )
                    .children(self.close_transfer_question(cx))
                    .child(self.destination_panel(cx)),
            )
    }
}

/// A path with the user's home folder written as `~`, which is how it is read.
///
/// The full path is still in the line's tooltip; this only keeps the visible line short
/// enough to be read at a glance instead of being cut off.
fn home_relative(path: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    match path.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.starts_with('/') || rest.starts_with('\\')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

/// The file name of a program path, which is what a person recognises it by.
///
/// "C:\\Windows\\System32\\cmd.exe" reads as "cmd.exe"; a bare name stays as it is.
fn program_label(program: &str) -> String {
    file_name(program).unwrap_or(program).to_owned()
}

/// The shortcut label of "new local shell" on one platform.
fn new_shell_shortcut_label(platform: Platform) -> &'static str {
    platform.new_shell_label()
}

/// The tone that carries a session state.
///
/// One place decides it, so a tab, a bar and a list row cannot disagree about whether
/// a session is live, still connecting or over.
fn session_tone(state: &SessionState) -> Tone {
    match state {
        SessionState::Connected => Tone::Positive,
        SessionState::Connecting | SessionState::VerifyingHost | SessionState::Authenticating => {
            Tone::Warning
        }
        // The reason decides, not just the state: a shell that exited with status 0
        // ended as asked, and the bar must not call that a failure (R12).
        SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
            Tone::for_failure(failure)
        }
        SessionState::Idle | SessionState::Closed => Tone::Neutral,
    }
}

/// Whether the manual reconnect can start another process for this session.
///
/// Two things have to hold: the state allows a restart at all, and the reason it ended
/// is one that another attempt can resolve. A refused reconnect carries its own reason,
/// so a session that can never be started again stops advertising the button that would
/// only refuse it again.
fn may_reconnect(session: &Session) -> bool {
    match session.state() {
        SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
            failure.reconnect_can_help()
        }
        state => state.can_reconnect(),
    }
}

/// What a session talks to, as the user sees it: the host's address and user, or the
/// program of a local shell.
///
/// A remote session keeps its host id, so a host that was renamed shows its new name and
/// a host that was deleted shows the id instead of an empty space.
fn session_target(book: &HostBook, session: &Session) -> String {
    match session.kind() {
        SessionKind::Local { shell } => shell.clone().unwrap_or_else(|| session.title().to_owned()),
        SessionKind::Remote { host } => book
            .host(*host)
            .map_or_else(|| format!("host {host}"), HostConfig::target),
    }
}

/// The store the shell reads when the caller does not supply one.
#[cfg(not(test))]
fn default_store() -> HostStore {
    HostStore::default_location()
}

/// A scratch store, so a test that mounts the whole shell never reads or writes the
/// developer's real data directory.
#[cfg(test)]
fn default_store() -> HostStore {
    scratch_store()
}

/// The credential store the shell keeps remembered secrets in.
#[cfg(not(test))]
fn default_secrets() -> std::sync::Arc<dyn SecretStore> {
    std::sync::Arc::new(shellune_storage::SystemSecrets)
}

/// Memory only, so a test never reads or writes the machine's credential store.
#[cfg(test)]
fn default_secrets() -> std::sync::Arc<dyn SecretStore> {
    std::sync::Arc::new(shellune_storage::MemorySecrets::new())
}

/// The trust file the shell reads when the caller does not supply one.
#[cfg(not(test))]
fn default_trust() -> TrustStore {
    TrustStore::default_location()
}

/// A trust file of this test process's own, next to the scratch host file.
#[cfg(test)]
fn default_trust() -> TrustStore {
    TrustStore::at(scratch_store().root())
}

/// A store in a directory of its own, unique to this test process.
#[cfg(test)]
pub(crate) fn scratch_store() -> HostStore {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let unique = format!(
        "shellune-app-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    HostStore::at(std::env::temp_dir().join(unique))
}

/// The icon a destination is drawn with in the sidebar.
fn destination_icon(destination: Destination) -> Icon {
    match destination {
        Destination::LocalShells => Icon::Terminal,
        Destination::Hosts => Icon::Server,
        Destination::Sessions => Icon::Layers,
        Destination::Transfers => Icon::Transfer,
    }
}

/// The product mark, shared with the native application icons.
fn brand_mark(size: gpui::Pixels, _metrics: Metrics) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(size)
        .h(size)
        .child(gpui::img(icons::APP_ICON).size(size))
}

/// A rounded square with the initials of a host, so a list is scanned by name shape.
///
/// A host that was really reached before is tinted in the positive tone, the same
/// rule the recent view uses for its dot; every other host is drawn in the accent.
fn host_avatar(name: &str, reached: bool, metrics: Metrics) -> Div {
    let initials: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .take(2)
        .filter_map(|part| part.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    let initials = if initials.is_empty() {
        "#".to_owned()
    } else {
        initials
    };
    let (background, text) = if reached {
        (palette::POSITIVE_TINT, palette::POSITIVE)
    } else {
        // A stable hue per name, so a list is told apart by colour before it is read.
        let hues = palette::AVATARS;
        let seed = name.bytes().fold(0usize, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(usize::from(byte))
        });
        hues[seed % hues.len()]
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(metrics.scale(36.0))
        .h(metrics.scale(36.0))
        .rounded(px(radius::MEDIUM + 2.0))
        .bg(theme::swatch(background))
        .text_size(metrics.label_size())
        .font_weight(FontWeight::BOLD)
        .text_color(theme::swatch(text))
        .child(SharedString::from(initials))
}

/// The group label a host row shows, when the host belongs to a group.
///
/// A host without a group shows nothing rather than the count unit, which used to
/// print the word "host" where a group name belongs.
pub(crate) fn host_group_chip(host: &HostConfig) -> Option<String> {
    host.group()
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned)
}

/// A muted line of explanation under a row or a heading.
fn caption_line(text: String, metrics: Metrics) -> Div {
    div()
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .child(SharedString::from(text))
}

/// One "line 3, line 7" list, for the lines a limit points at.
fn line_list(lines: &[usize]) -> String {
    lines
        .iter()
        .map(|line| format!("line {line}"))
        .collect::<Vec<String>>()
        .join(", ")
}
