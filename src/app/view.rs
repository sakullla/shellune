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
    Context, Div, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent, Render, SharedString,
    Stateful, Window, div, px,
};
use std::collections::HashMap;

use shellune_core::{
    BrowseFailure, BrowseState, EventTag, FileKind, ForwardId, ForwardRule, HostConfig, HostId,
    HostValidationError, LocalShell, Session, SessionFailure, SessionId, SessionKind, SessionState,
    SplitAxis, TabAccent, TabId, Transfer, TransferDirection, TransferProtocol, join_path,
};
use shellune_ssh::PtySize;
#[cfg(test)]
use shellune_ssh::RemoteEvent;
use shellune_storage::{
    BookError, ForwardStore, HostBook, HostStore, HostTrust, ImportOutcome, ImportPreview,
    ImportedHost, PreferencesProblem, PreferencesStore, SecretStore, SnippetStore, StorageError,
    TrustStore,
};
use shellune_terminal::GridSize;

use crate::app::actions::{
    CloseActiveTab, CycleTextSize, DismissNotice, FocusPaneDown, FocusPaneLeft, FocusPaneRight,
    FocusPaneUp, GoToForwards, GoToHosts, GoToLocalShells, GoToSessions, GoToSettings,
    GoToTransfers, MovePaneDown, MovePaneLeft, MovePaneRight, MovePaneUp, MoveTabLeft,
    MoveTabRight, NewLocalShell, NextDestination, NextTab, OpenPalette, PreviousDestination,
    PreviousTab, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, SelectTab1,
    SelectTab2, SelectTab3, SelectTab4, SelectTab5, SelectTab6, SelectTab7, SelectTab8, SelectTab9,
    SplitPaneDown, SplitPaneRight, ToggleLanguage, ToggleSidebar, ZoomPane,
};
use crate::app::components::Variant;
use crate::app::copy::{self, Locale, Strings};
#[cfg(test)]
use crate::app::files::BrowseChannel;
use crate::app::files::{
    FileSource, FileViews, FilesDialog, browse_failure_text, display_path, kind_label,
    local_protocol_to_os, path_crumbs, path_problem_text, problem_text, transfer_problem_text,
};
use crate::app::forwarding::ForwardCenter;
use crate::app::hosts::{
    HostDraft, HostField, HostView, HostsPanel, ImportPanel, format_unix, group_row_offsets,
    import_message, unimported_hosts,
};
use crate::app::icons::{self, Icon};
use crate::app::keymap::{self, Keymap, Problem};
use crate::app::nav::{Destination, Platform};
use crate::app::remote::{
    self, AttemptLink, ChainDraft, LinkEvent, MethodChoice, RemoteFailure, RemotePanel,
    RemoteStatus,
};
use crate::app::snippets::{SnippetLibrary, SnippetsPanel};
use crate::app::terminal::{self, PaneState, SourceEvent, TerminalAttention, TerminalPane};
use crate::app::text_field::{Browse, TextField};
use crate::app::theme::{self, Metrics, Tone, palette, spacing};
#[cfg(test)]
use crate::app::transfers::TransferChannel;
use crate::app::transfers::{
    PathProblem, TransferProblem, TransferViews, default_download_directory, file_name,
};
use crate::app::workspace::{
    AttentionKind, Failure, Recovery, SectionState, SectionStatus, UnreadNotices, Workspace,
};
use crate::app::{components, theme::radius};
#[cfg(test)]
use shellune_storage::KeymapPreferences;

mod files_menu;
mod forwarding;
mod host_dialog;
mod overlays;
mod panes;
mod sessions;
mod shell;
mod titlebar;
mod toasts;

#[cfg(test)]
pub use shell::Tool;
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

/// The corner note that says a saved command waits in one pane's paste prompt (R6).
///
/// The note is kept only while the prompt it describes is still open. Answering the
/// prompt confirms or cancels the command, and the note leaves with it, so the corner
/// never keeps saying a command is waiting when nothing waits any more. Only one note is
/// tracked: the newest command asked for is the one the corner speaks about, and asking
/// for another replaces it.
#[derive(Clone, Copy)]
struct SnippetNote {
    /// The session whose terminal holds the command.
    session: SessionId,
    /// The toast that says so, so it can be taken away before it expires.
    toast: u64,
}

/// A file dragged from one column onto the other.
///
/// The directory is the one the file was listed in, so the drop asks to transfer
/// that file and not whatever now occupies the same row.
#[derive(Clone)]
struct FileDrag {
    session: SessionId,
    source: FileSource,
    name: String,
    directory: String,
}

/// The small label GPUI shows while a file is dragged.
struct FileDragLabel {
    name: SharedString,
}

impl Render for FileDragLabel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(radius::SMALL))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .text_size(px(12.0))
            .child(self.name.clone())
    }
}

/// Width of a file row's action column: one square per action that source offers,
/// plus the gaps between them.
///
/// The column used to be a fixed 96 wide, which no longer holds four 24×24 targets:
/// the row's buttons would spill out of it and cover the size next to them (R4). The
/// header uses the same width, so the size column lines up down the list.
fn files_actions_width(metrics: Metrics, source: FileSource) -> gpui::Pixels {
    // A remote file shows download, send back, rename and delete; a local file shows
    // send, rename and delete. A folder shows only the last two, and keeps the slots
    // of the wider rows so the columns stay in one place.
    let slots = match source {
        FileSource::Remote => 4.0,
        FileSource::Local => 3.0,
    };
    let slot = f32::from(metrics.hit_target());
    let gap = f32::from(metrics.scale(1.0));
    px(slot * slots + gap * (slots - 1.0))
}

/// A debug id for one file source. The remote column keeps the ids the file view
/// already used; the local column is a different object and does not reuse them.
fn files_element_id(source: FileSource, suffix: &str) -> SharedString {
    match source {
        FileSource::Remote => SharedString::from(format!("files.{suffix}")),
        FileSource::Local => SharedString::from(format!("files.local.{suffix}")),
    }
}

/// A divider drag. The listener is registered again on every frame it lasts.
#[derive(Clone, Copy)]
struct SplitDrag {
    split: u64,
    axis: SplitAxis,
    origin: gpui::Point<gpui::Pixels>,
    origin_ratio: u16,
}

/// One connection attempt.
///
/// Several may be in flight together. Each owns its session, its transport and, when
/// it is not the form on screen, its own panel. Cancelling one does not touch the
/// others. `link` is `None` after the transport has ended, or while a form is parked
/// before it dials, and the form can still show the reason.
struct Attempt {
    /// The session record the attempt owns.
    id: SessionId,
    /// The session generation this transport belongs to.
    generation: u32,
    /// The transport, until it becomes a terminal or ends.
    link: Option<AttemptLink>,
    /// Whether a poll loop is already watching it.
    watching: bool,
    /// The form while another attempt is the one on screen.
    ///
    /// `None` means the form is [`ShelluneView::remote`] because this attempt is focused.
    panel: Option<RemotePanel>,
    /// The jump chain this form is filling in, while another form is on screen.
    chain: Option<ChainDraft>,
    /// A trust answer queued for a chain hop, written only after that hop is ready.
    pending_trust: Option<PendingTrust>,
    /// The hop whose trust or ProxyCommand question is on screen.
    asking_hop: Option<HostId>,
    /// The host the session record is for. A form may be asking an earlier hop.
    target: HostConfig,
    /// The host was not written to the host file.
    ephemeral: bool,
    /// This dial must not open a shell channel.
    forward_only: bool,
    /// Rules to start once this dial authenticates.
    pending_rules: Vec<ForwardId>,
}

/// The facts a connected attempt still needs after its transport has been taken.
struct ConnectedSession {
    host: HostConfig,
    ephemeral: bool,
    show: bool,
    forward_only: bool,
    pending_rules: Vec<ForwardId>,
}

/// An accepted chain identity that has not been written yet.
struct PendingTrust {
    host_id: HostId,
    prompt: shellune_ssh::HostKeyPrompt,
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

/// Preferred width of one file column, before text scale.
///
/// Beside rather than above: a directory list and a terminal are two things a user
/// reads at the same time, and stacking them leaves neither enough rows. The drawn
/// width is this length scaled by the text size, then clamped to the content area
/// when only one column fits.
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
    /// The file view's right-click menu, while open.
    files_menu: Option<files_menu::FilesMenu>,
    /// Short messages in the corner, oldest first.
    toasts: Vec<toasts::Toast>,
    next_toast: u64,
    /// The note that says a saved command waits for confirmation in a pane's prompt.
    snippet_note: Option<SnippetNote>,
    /// The pointer is over the toasts, which holds them while they are read.
    toasts_hovered: bool,
    /// Where the header's "▾" was last drawn, so its menu can hang from it.
    shell_menu_anchor: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
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
    /// The jump chain the open form is collecting. Parked with the form.
    chain: Option<ChainDraft>,
    /// Every connection attempt that has not become a terminal, focused or not.
    attempts: Vec<Attempt>,
    /// The session the open form belongs to, once an attempt exists for it.
    form_session: Option<SessionId>,
    /// The quick-connection form is open.
    quick_open: bool,
    /// Why the quick-connection form refused to start.
    quick_error: Option<String>,
    /// The sign-in method the quick-connection form is set to.
    quick_method: MethodChoice,
    /// Address, port, user and the secret the chosen method still needs.
    quick_address: Entity<TextField>,
    quick_port: Entity<TextField>,
    quick_user: Entity<TextField>,
    quick_password: Entity<TextField>,
    quick_key: Entity<TextField>,
    quick_passphrase: Entity<TextField>,
    /// Host used by the next [`Self::connect_remote`] when it must not be saved yet.
    pending_ephemeral: Option<HostConfig>,
    /// Successful quick connections the user has not saved as hosts.
    unsaved: Vec<(SessionId, HostConfig)>,
    /// The last explicit save of a quick connection, shown on the session bar.
    quick_save_notice: Option<QuickSaveNotice>,
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
    /// Saved forwarding rules and the listeners a connection is actually running.
    forwards: ForwardCenter,
    /// The saved shortcut commands and the file behind them (R6).
    snippets: SnippetLibrary,
    /// The manager of those commands: its editor, its deletion question and its row.
    snippet_panel: SnippetsPanel,
    /// The field of the command editor that takes the keyboard on the next draw.
    ///
    /// The dialog is opened from a button or the palette, neither of which has a window to
    /// give the keyboard with, so the focus is handed over where a window exists.
    snippet_focus_pending: Option<FocusHandle>,
    /// The manager's own keyboard target, so its list answers the arrow keys while no
    /// field of the editor is open.
    snippet_focus: FocusHandle,
    forward_listen_host: Entity<TextField>,
    forward_listen_port: Entity<TextField>,
    forward_target_host: Entity<TextField>,
    forward_target_port: Entity<TextField>,
    /// Whether forward events are being polled.
    forwards_polling: bool,
    /// The next dial is a pure forward and must not open a shell.
    forward_only_dial: bool,
    /// Rules waiting for that dial to authenticate.
    pending_forward_rules: Vec<ForwardId>,
    /// A non-loopback listen waiting for confirmation. Nothing is bound.
    pending_exposure: Option<ForwardId>,
    /// Pure-forward connections, kept alive without a terminal pane.
    forward_carriers: HashMap<SessionId, AttemptLink>,
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
    /// The application close is waiting because transfers are still running.
    closing_app: bool,
    /// The user confirmed that those transfers may stop, so the window may close.
    app_close_confirmed: bool,
    /// Whether the window-close callback has been registered.
    watching_close: bool,
    /// Whether the file views are being polled for answers.
    files_polling: bool,
    /// The change the file view is asking about before anything is sent (R11).
    files_dialog: Option<FilesDialog>,
    /// The field of that question, present only while it has one.
    files_field: Option<Entity<TextField>>,
    /// The remote path field of a transfer question, when it has one (R10).
    files_remote_field: Option<Entity<TextField>>,
    /// Focus of the local column, so its keys are not the terminal's keys.
    files_local_focus: FocusHandle,
    /// Focus of the remote column.
    files_remote_focus: FocusHandle,
    /// Last viewport width, which chooses two columns or one.
    viewport_width: gpui::Pixels,
    /// Forces the file layout in a test. `None` follows the viewport.
    files_layout_override: Option<bool>,
    /// Where remembered passwords and passphrases are kept: the system credential store,
    /// or memory in tests.
    secrets: std::sync::Arc<dyn SecretStore>,
    /// The command palette's search field, present only while the palette is open.
    palette: Option<Entity<TextField>>,
    /// What the palette's field held when the list was last ranked.
    palette_query: String,
    /// Whether the last input was the pointer. A focus ring answers "where are the
    /// keys?", which only matters to someone using them; a click already shows
    /// where it landed.
    pointer_input: bool,
    /// The host tool open over the page, if any.
    tool: Option<shell::Tool>,
    /// A terminal that should have the keyboard by the next draw.
    pane_focus_pending: Option<SessionId>,
    /// Gives the keyboard back to the page when the focused control leaves the
    /// screen (a dialog that closed, a form that was replaced).
    focus_lost_watch: Option<gpui::Subscription>,
    /// The highlighted palette entry.
    palette_selected: usize,
    /// Keeps the highlighted palette entry in view.
    palette_scroll: gpui::ScrollHandle,
    /// The tab whose menu is open, and where the pointer opened it.
    tab_menu: Option<(SessionId, gpui::Point<gpui::Pixels>)>,
    /// The tab whose name is being edited, while that dialog is open.
    tab_rename: Option<TabRename>,
    /// The colour swatch the pointer is on, and which of the two rows it is in.
    ///
    /// The swatches draw their hover and press states from this, so the same value
    /// the pointer sets is the one the drawing reads — and the one a test can see.
    accent_hover: Option<(AccentRow, TabAccent)>,
    /// Whether the list of every open tab is open.
    tab_list_open: bool,
    /// Whether a user edit may be written. Window close turns this off first.
    layout_persist: bool,
    /// Why the saved layout could not be read. The file is left as it was.
    layout_error: Option<String>,
    /// The split whose divider is being dragged.
    split_drag: Option<SplitDrag>,
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
    /// The terminal's palette: the window's, or always dark or light.
    terminal_scheme: theme::TerminalScheme,
    /// The preferences last written (or read), so a draw only writes a change.
    /// `None` while preferences are not kept, as in the unit tests, which share
    /// one scratch directory per process.
    saved_preferences: Option<shellune_storage::Preferences>,
    /// The key bindings in force: the catalogue's defaults with the user's changes
    /// (R5). The command palette and every hint read their shortcut from here.
    keymap: Keymap,
    /// Why the saved bindings were not applied. The settings page says so instead of
    /// letting the user believe the defaults are their own choice.
    keymap_problem: Option<Problem>,
    /// The action whose new shortcut is being captured, while the layer is open.
    key_capture: Option<KeyCapture>,
    /// The keystroke interceptor that reads a captured shortcut before anything else
    /// resolves it. Dropped when the layer closes.
    capture_watch: Option<gpui::Subscription>,
    /// Why the last shortcut change could not be written to the preferences file. The
    /// change still holds for this run; the settings page says it may not survive a
    /// restart rather than pretending it was saved.
    keymap_save_error: Option<String>,
    /// The settings page's action search field.
    keys_search: Entity<TextField>,
    /// The shortcut row the keyboard is on, which Enter opens for capture.
    ///
    /// The page is navigable with the arrow keys so a binding can be changed, unbound
    /// or restored without a pointer (R4, R5); the row the keyboard is on is the one
    /// the ring is drawn on.
    keys_selected: usize,
    /// Which of that row's controls the keyboard is on: they are the same three the
    /// pointer sees, in the order [`key_row_controls`] gives them.
    keys_control: usize,
    /// Whether the window already tells this view when the system palette changes.
    watching_appearance: bool,
    /// Whether the host dialog's first field takes the keyboard on the next draw.
    focus_host_dialog: bool,
    /// Focus to return to when an overlay closes. Weak, so a removed field is not kept.
    focus_return: Option<gpui::WeakFocusHandle>,
    /// The next draw gives the keyboard back to [`Self::focus_return`].
    restore_focus_pending: bool,
    /// The session row whose menu is open, and where it was opened.
    session_menu: Option<(SessionId, gpui::Point<gpui::Pixels>)>,
    /// Attempts a test scripted, kept so their answers can be inspected.
    #[cfg(test)]
    scripted: Vec<(SessionId, shellune_ssh::ScriptedSession)>,
}

/// The last explicit save of a quick connection, drawn on the session bar.
#[derive(Clone)]
struct QuickSaveNotice {
    failed: bool,
    reason: String,
    next: String,
}

/// The shortcut capture layer: which action is waiting for a key, and why the last
/// key it saw was refused.
struct KeyCapture {
    action: &'static str,
    problem: Option<Problem>,
}

/// The tab-name dialog: which tab is being named, its field, and why the last name
/// was refused.
///
/// The field is made per opening so its placeholder is in the current language and no
/// earlier name is waiting in it. The dialog names a tab; it does not touch the
/// session, its target or its state.
struct TabRename {
    /// The tab being named.
    tab: TabId,
    /// The session it holds, when it has one, for the dialog's subtitle.
    session: Option<SessionId>,
    /// The name the user typed, on its way to the tab.
    field: Entity<TextField>,
    /// Why the entered name could not be used, when it could not.
    problem: Option<String>,
    /// Whether the last save cut the name to the length a tab can hold.
    ///
    /// The dialog stays open in that case, holding the name that was kept, so the
    /// shortening is read rather than missed between a keystroke and a closed window.
    shortened: bool,
}

/// One of the two rows of colour swatches a tab can be coloured from.
///
/// Both rows draw the same swatches and answer the pointer the same way; the row only
/// names which surface a hovered swatch belongs to, so the state a test reads and the
/// debug selector agree with the row on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccentRow {
    /// The swatches in the tab's own menu.
    Menu,
    /// The swatches in the tab-name dialog.
    Rename,
}

impl AccentRow {
    /// The prefix of the debug selectors of this row's swatches.
    #[must_use]
    pub(crate) fn selector(self) -> &'static str {
        match self {
            Self::Menu => "tab.menu",
            Self::Rename => "tab.rename",
        }
    }
}

/// Which line the tab-name dialog carries about the length a tab can hold.
///
/// A name past the limit is cut rather than refused, so the dialog says so: while the
/// field holds too much, and again when a save kept only the first characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabRenameLimit {
    /// The field holds a name longer than a tab keeps.
    Typing,
    /// The last save kept only the first characters of the name.
    Saved,
}

/// One control of a shortcut row.
///
/// The row offers the same three controls to the pointer and to the keyboard, and
/// which ones exist depends on the action: an unbound action cannot be unbound again,
/// and an action at its default has nothing to restore (R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyRowControl {
    /// Open the capture layer and take the next combination.
    Change,
    /// Hand the keys back to whatever has the keyboard.
    Unbind,
    /// Put the action back on its default bindings.
    Reset,
}

/// The controls a shortcut row offers, in the order they are drawn and stepped.
pub(crate) fn key_row_controls(bound: bool, default: bool) -> Vec<KeyRowControl> {
    let mut controls = vec![KeyRowControl::Change];
    if bound {
        controls.push(KeyRowControl::Unbind);
    }
    if !default {
        controls.push(KeyRowControl::Reset);
    }
    controls
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
    proxy: Entity<TextField>,
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
            HostField::ProxyCommand => &self.proxy,
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
        let keys_search = cx.new(|cx| TextField::new(strings.keys_search_placeholder, cx));

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
        let quick_key = cx.new(|cx| {
            let mut field = TextField::new(strings.auth_key, cx);
            field.set_browse(Browse::File, cx);
            field
        });
        let quick_passphrase = cx.new(|cx| {
            let mut field = TextField::new(strings.auth_key_passphrase, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        // Examples, not labels: the form names each field above it, and an address
        // reads the same in every language.
        let forward_listen_host = cx.new(|cx| TextField::new("127.0.0.1", cx));
        let forward_listen_port = cx.new(|cx| TextField::new("8080", cx));
        let forward_target_host = cx.new(|cx| TextField::new("db.internal", cx));
        let forward_target_port = cx.new(|cx| TextField::new("5432", cx));
        // The saved commands live beside the hosts, in the same data directory. Reading
        // them here starts nothing; a damaged file is kept and reported by the library.
        let snippet_store = SnippetStore::at(store.root());

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
            files_menu: None,
            toasts: Vec::new(),
            next_toast: 0,
            snippet_note: None,
            toasts_hovered: false,
            shell_menu_anchor: std::rc::Rc::default(),
            store,
            book: HostBook::new(),
            hosts: HostsPanel::new(),
            hosts_search,
            host_fields: None,
            import: ImportPanel::new(),
            import_path: None,
            remote: None,
            chain: None,
            attempts: Vec::new(),
            form_session: None,
            quick_open: false,
            quick_error: None,
            quick_method: MethodChoice::Password,
            quick_address,
            quick_port,
            quick_user,
            quick_password,
            quick_key,
            quick_passphrase,
            pending_ephemeral: None,
            unsaved: Vec::new(),
            quick_save_notice: None,
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
            forwards: ForwardCenter::new(),
            snippets: SnippetLibrary::load(snippet_store),
            snippet_panel: SnippetsPanel::new(),
            snippet_focus_pending: None,
            snippet_focus: cx.focus_handle(),
            forward_listen_host,
            forward_listen_port,
            forward_target_host,
            forward_target_port,
            forwards_polling: false,
            forward_only_dial: false,
            pending_forward_rules: Vec::new(),
            pending_exposure: None,
            forward_carriers: HashMap::new(),
            transfer_problem: None,
            closing: None,
            closing_app: false,
            app_close_confirmed: false,
            watching_close: false,
            files_polling: false,
            files_dialog: None,
            files_field: None,
            files_remote_field: None,
            files_local_focus: cx.focus_handle(),
            files_remote_focus: cx.focus_handle(),
            viewport_width: px(crate::app::DEFAULT_WINDOW_SIZE.0),
            files_layout_override: None,
            palette: None,
            palette_query: String::new(),
            pointer_input: false,
            tool: None,
            pane_focus_pending: None,
            focus_lost_watch: None,
            palette_selected: 0,
            palette_scroll: gpui::ScrollHandle::new(),
            tab_menu: None,
            tab_rename: None,
            accent_hover: None,
            tab_list_open: false,
            session_menu: None,
            focus_host_dialog: false,
            focus_return: None,
            restore_focus_pending: false,
            rail_collapsed: false,
            theme_choice: theme::ThemeChoice::default(),
            terminal_scheme: theme::TerminalScheme::default(),
            saved_preferences: None,
            keymap: Keymap::defaults(Platform::current()),
            keymap_problem: None,
            key_capture: None,
            capture_watch: None,
            keymap_save_error: None,
            keys_search,
            keys_selected: 0,
            keys_control: 0,
            watching_appearance: false,
            layout_persist: false,
            layout_error: None,
            split_drag: None,
            session_terminal: false,
            #[cfg(test)]
            scripted: Vec::new(),
        };
        view.bind_cleanup(cx);
        view.bind_shell_path(cx);
        view.bind_host_search(cx);
        view.bind_keys_search(cx);
        view.bind_remote_fields(cx);
        view.restore_hosts(cx);
        view.restore_trust();
        view.reload_forwards();
        view.restore_workspace_layout();
        view.restore_preferences(cx);
        view.relabel_fields(cx);
        view
    }

    /// Applies the saved theme, language, text size, motion, sidebar, terminal font
    /// size and key bindings. Unit tests keep the defaults, so one test's choice
    /// cannot leak into the next.
    ///
    /// The key bindings are applied to the window here, before the first frame: a
    /// saved change has to be in force from the start, not after the user touches the
    /// settings page again.
    fn restore_preferences(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        let (saved, problem) = PreferencesStore::at(self.store.root()).load_reported();
        self.apply_preferences(saved, problem, cx);
    }

    /// Applies a preferences document to the running window.
    ///
    /// The reader and the tests' own restart both come through here, so what a test
    /// checks after a "restart" is the same code a real start runs.
    fn apply_preferences(
        &mut self,
        saved: shellune_storage::Preferences,
        problem: Option<PreferencesProblem>,
        cx: &mut Context<Self>,
    ) {
        let platform = Platform::current();
        let (keymap, keymap_problem) = Keymap::resolve(platform, saved.keymap.as_ref());
        let keymap_problem = match problem {
            Some(PreferencesProblem::Unreadable) => Some(Problem::UnreadablePreferences),
            None => keymap_problem,
        };
        self.keymap = keymap;
        self.keymap_problem = keymap_problem;
        crate::app::apply_keymap(cx, &self.keymap);
        if let Some(choice) = saved.theme.as_deref().and_then(|value| match value {
            "system" => Some(theme::ThemeChoice::System),
            "light" => Some(theme::ThemeChoice::Light),
            "dark" => Some(theme::ThemeChoice::Dark),
            _ => None,
        }) {
            self.theme_choice = choice;
        }
        if let Some(locale) = saved.language.as_deref().and_then(|value| match value {
            "en" => Some(Locale::English),
            "zh" => Some(Locale::Chinese),
            _ => None,
        }) {
            self.workspace.set_locale(locale);
        }
        if let Some(zoom) = saved.text_size.and_then(|percent| {
            theme::Zoom::ALL
                .into_iter()
                .find(|zoom| zoom_percent(*zoom) == percent)
        }) {
            self.workspace.set_zoom(zoom);
        }
        if let Some(motion) = saved.motion.as_deref().and_then(|value| match value {
            "full" => Some(theme::Motion::Full),
            "reduced" => Some(theme::Motion::Reduced),
            _ => None,
        }) {
            self.workspace.set_motion(motion);
        }
        if let Some(scheme) = saved
            .terminal_scheme
            .as_deref()
            .and_then(|value| match value {
                "app" => Some(theme::TerminalScheme::FollowApp),
                "dark" => Some(theme::TerminalScheme::Dark),
                "light" => Some(theme::TerminalScheme::Light),
                _ => None,
            })
        {
            self.terminal_scheme = scheme;
        }
        if let Some(collapsed) = saved.sidebar_collapsed {
            self.rail_collapsed = collapsed;
        }
        if let Some(size) = saved
            .terminal_font_size
            .filter(|size| size.is_finite() && (6.0..=72.0).contains(size))
        {
            self.workspace.set_local_font_size(size);
        }
        // A reminders file written before these fields existed keeps both on, which is
        // what the defaults say; a file that names one applies it (R7).
        if let Some(bell) = saved.bell_notice {
            self.workspace.set_attention(AttentionKind::Bell, bell);
        }
        if let Some(command) = saved.command_notice {
            self.workspace
                .set_attention(AttentionKind::CommandFinished, command);
        }
        self.saved_preferences = Some(self.current_preferences());
    }

    /// The preferences as they stand now.
    fn current_preferences(&self) -> shellune_storage::Preferences {
        shellune_storage::Preferences {
            theme: Some(
                match self.theme_choice {
                    theme::ThemeChoice::System => "system",
                    theme::ThemeChoice::Light => "light",
                    theme::ThemeChoice::Dark => "dark",
                }
                .to_owned(),
            ),
            language: Some(
                match self.workspace.locale() {
                    Locale::English => "en",
                    Locale::Chinese => "zh",
                }
                .to_owned(),
            ),
            text_size: Some(zoom_percent(self.workspace.zoom())),
            motion: Some(
                if self.workspace.motion().allows_animation() {
                    "full"
                } else {
                    "reduced"
                }
                .to_owned(),
            ),
            sidebar_collapsed: Some(self.rail_collapsed),
            terminal_font_size: Some(self.workspace.local_font_size()),
            terminal_scheme: Some(
                match self.terminal_scheme {
                    theme::TerminalScheme::FollowApp => "app",
                    theme::TerminalScheme::Dark => "dark",
                    theme::TerminalScheme::Light => "light",
                }
                .to_owned(),
            ),
            keymap: {
                // An untouched key map stays out of the file: the catalogue's defaults
                // are the truth, and writing them down would freeze this build's
                // choices into the user's data. Another platform's bindings are kept:
                // this build only writes its own section of the shared file.
                let saved = self
                    .saved_preferences
                    .as_ref()
                    .and_then(|saved| saved.keymap.as_ref());
                let keymap = self.keymap.to_preferences(saved);
                (!keymap.is_empty()).then_some(keymap)
            },
            // Both reminders are written even when they are on, so a file says what the
            // user chose rather than leaving the next start to assume (R7).
            bell_notice: Some(self.workspace.attention_settings().bell),
            command_notice: Some(self.workspace.attention_settings().finished),
        }
    }

    /// Writes the preferences when one changed since the last write. Called from
    /// every draw, so the sidebar, the palette, the shortcuts and the key bindings
    /// all persist through one path. A failed write is not worth a banner: the
    /// choice still holds for this run, and the next change tries again.
    fn persist_preferences(&mut self) {
        let Some(saved) = &self.saved_preferences else {
            return;
        };
        let now = self.current_preferences();
        if *saved == now {
            return;
        }
        let _ = PreferencesStore::at(self.store.root()).save(&now);
        self.saved_preferences = Some(now);
    }

    // -----------------------------------------------------------------------
    // Keyboard shortcuts (R5)
    // -----------------------------------------------------------------------

    /// The bindings in force. The palette, the hints and the settings page read their
    /// shortcut from here, so they can never disagree with what the window answers.
    #[cfg(test)]
    #[must_use]
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// The settings page's action search, for the tests that check the list narrows.
    #[cfg(test)]
    #[must_use]
    pub fn keys_query(&self, cx: &gpui::App) -> String {
        self.keys_search.read(cx).text().to_owned()
    }

    /// Why the saved bindings were not applied; `None` when they were.
    #[cfg(test)]
    #[must_use]
    pub fn keymap_problem(&self) -> Option<&Problem> {
        self.keymap_problem.as_ref()
    }

    /// Whether the capture layer is open, and for which action.
    #[cfg(test)]
    #[must_use]
    pub fn capturing_for(&self) -> Option<&'static str> {
        self.key_capture.as_ref().map(|capture| capture.action)
    }

    /// Opens the capture layer for one action: the next combination pressed becomes
    /// its shortcut.
    fn keys_capture_start(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        // Interception, not a key listener: a combination already bound to another
        // action has to be seen here first, otherwise it would run that action and
        // the capture layer would never learn what was pressed.
        let watch = cx.intercept_keystrokes(move |event, _window, app| {
            let keystroke = event.keystroke.clone();
            let taken = view
                .update(app, |view, cx| view.keys_capture_key(&keystroke, cx))
                .unwrap_or(false);
            if taken {
                app.stop_propagation();
            }
        });
        self.key_capture = Some(KeyCapture {
            action,
            problem: None,
        });
        self.capture_watch = Some(watch);
        cx.notify();
    }

    /// Closes the capture layer without changing anything.
    fn keys_capture_cancel(&mut self, cx: &mut Context<Self>) {
        self.key_capture = None;
        self.capture_watch = None;
        cx.notify();
    }

    /// Reads one keystroke while the capture layer is open. Returns whether it was
    /// consumed.
    fn keys_capture_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(capture) = self.key_capture.as_ref() else {
            return false;
        };
        let action = capture.action;
        if keystroke.key == "escape" && keystroke.modifiers.number_of_modifiers() == 0 {
            self.keys_capture_cancel(cx);
            return true;
        }
        // A modifier on its own is not a shortcut yet: keep waiting for the key that
        // is pressed with it.
        if matches!(
            keystroke.key.as_str(),
            "shift" | "control" | "alt" | "platform" | "super" | "cmd" | "function" | "win"
        ) {
            return true;
        }
        let candidate = keystroke.unparse();
        match self.keymap.set(action, &candidate) {
            Ok(()) => {
                self.key_capture = None;
                self.capture_watch = None;
                self.keymap_changed(cx);
            }
            Err(problem) => {
                if let Some(capture) = self.key_capture.as_mut() {
                    capture.problem = Some(problem);
                }
                cx.notify();
            }
        }
        true
    }

    /// Hands one action's keys back to whatever has the keyboard: with no binding the
    /// combination reaches the terminal program (R5).
    fn keys_unbind(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.keymap.unbind(action);
        if let Some(capture) = self.key_capture.as_mut() {
            capture.problem = None;
        }
        self.keymap_changed(cx);
    }

    /// Returns one action to its default shortcut.
    fn keys_reset(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.keymap.reset(action);
        if let Some(capture) = self.key_capture.as_mut() {
            capture.problem = None;
        }
        self.keymap_changed(cx);
    }

    /// Returns every action to its default shortcut.
    fn keys_reset_all(&mut self, cx: &mut Context<Self>) {
        self.keymap.reset_all();
        self.keymap_problem = None;
        self.keymap_changed(cx);
    }

    /// Applies a changed key map to the window and writes it down.
    ///
    /// The bindings are rebuilt from the map, so an unbound action really stops
    /// answering. A write that fails is kept visible instead of being reported as a
    /// saved preference: the change holds for this run, the file on disk does not have
    /// it, and the settings page says so.
    fn keymap_changed(&mut self, cx: &mut Context<Self>) {
        self.keymap_problem = None;
        crate::app::apply_keymap(cx, &self.keymap);
        // The terminals' own toolbars name the shortcuts, so they follow the change.
        let shortcuts = terminal::TerminalShortcuts::of(&self.keymap);
        for open in &self.panes {
            let shortcuts = shortcuts.clone();
            open.pane
                .update(cx, |pane, cx| pane.set_shortcuts(shortcuts, cx));
        }
        if self.saved_preferences.is_some() {
            let now = self.current_preferences();
            match PreferencesStore::at(self.store.root()).save(&now) {
                Ok(()) => {
                    self.saved_preferences = Some(now);
                    self.keymap_save_error = None;
                }
                Err(error) => self.keymap_save_error = Some(error.to_string()),
            }
        }
        cx.notify();
    }

    /// Reads a saved key map as if it had just been loaded, for the tests that observe
    /// what a damaged file looks like on the settings page.
    #[cfg(test)]
    pub fn use_saved_keymap_for_test(&mut self, saved: &KeymapPreferences, cx: &mut Context<Self>) {
        let (keymap, problem) = Keymap::resolve(Platform::current(), Some(saved));
        self.keymap = keymap;
        self.keymap_problem = problem;
        crate::app::apply_keymap(cx, &self.keymap);
        cx.notify();
    }

    /// Applies a preferences document as a start would read it, for the tests that
    /// check a choice survives a restart.
    #[cfg(test)]
    pub fn use_saved_preferences_for_test(
        &mut self,
        saved: &shellune_storage::Preferences,
        cx: &mut Context<Self>,
    ) {
        self.apply_preferences(saved.clone(), None, cx);
        cx.notify();
    }

    /// The preferences this window would write now, for the tests that check a choice
    /// is recorded rather than merely applied.
    #[cfg(test)]
    #[must_use]
    pub fn current_preferences_for_test(&self) -> shellune_storage::Preferences {
        self.current_preferences()
    }

    /// The row the keyboard is on among the shortcut rows listed now.
    fn selected_key_row(&self, cx: &gpui::App) -> Option<&'static keymap::ActionSpec> {
        let query = self.keys_search.read(cx).text().to_owned();
        let matching = self.keymap.matching(&query, self.workspace.locale());
        if matching.is_empty() {
            return None;
        }
        matching
            .get(self.keys_selected.min(matching.len() - 1))
            .copied()
    }

    /// Moves the keyboard's row and keeps the highlight on a row that exists.
    fn step_key_row(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.keys_search.read(cx).text().to_owned();
        let count = self.keymap.matching(&query, self.workspace.locale()).len();
        self.keys_selected = crate::app::palette::step(self.keys_selected, delta, count);
        // The row that is now under the ring may offer fewer controls than the one
        // before it; the cursor stays on the last one it has.
        let controls = self.selected_key_row_controls(cx).len();
        self.keys_control = self.keys_control.min(controls.saturating_sub(1));
        cx.notify();
    }

    /// The controls the row under the keyboard offers, in the order the pointer shows
    /// them: the same list the row is drawn from.
    fn selected_key_row_controls(&self, cx: &gpui::App) -> Vec<KeyRowControl> {
        let Some(spec) = self.selected_key_row(cx) else {
            return Vec::new();
        };
        let bound = !self.keymap.is_unbound(spec.id);
        let default = self.keymap.is_default(spec.id);
        key_row_controls(bound, default)
    }

    /// Which control of the row under the keyboard has the keyboard's cursor.
    #[cfg(test)]
    #[must_use]
    pub fn key_row_control(&self, cx: &gpui::App) -> Option<KeyRowControl> {
        let controls = self.selected_key_row_controls(cx);
        controls
            .get(self.keys_control.min(controls.len().saturating_sub(1)))
            .copied()
    }

    /// Moves the keyboard's cursor along the controls of the row it is on.
    fn step_key_control(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.selected_key_row_controls(cx).len();
        if count == 0 {
            return;
        }
        let current = self.keys_control.min(count - 1);
        self.keys_control = crate::app::palette::step(current, delta, count);
        cx.notify();
    }

    /// Runs the control the keyboard's cursor is on: change, unbind or restore.
    ///
    /// This is the keyboard path through the settings page (R4, R5): the arrow keys
    /// choose a row and then one of its controls, and Enter does what a click on that
    /// control does.
    fn run_key_row_control(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.selected_key_row(cx) else {
            return;
        };
        let controls = self.selected_key_row_controls(cx);
        let control = controls
            .get(self.keys_control.min(controls.len().saturating_sub(1)))
            .copied();
        let action = spec.id;
        match control {
            Some(KeyRowControl::Change) => self.keys_capture_start(action, cx),
            Some(KeyRowControl::Unbind) => self.keys_unbind(action, cx),
            Some(KeyRowControl::Reset) => self.keys_reset(action, cx),
            None => (),
        }
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

    /// How many forwarding rules are saved, and whether any is listening.
    #[cfg(test)]
    pub fn forward_restore_state(&self) -> (usize, bool) {
        (
            self.forwards.book().rules().len(),
            self.forwards.any_active(),
        )
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

    /// The saved shortcut commands, for the tests that check what a restart reads (R6).
    #[allow(dead_code)]
    #[must_use]
    pub fn snippets(&self) -> &SnippetLibrary {
        &self.snippets
    }

    /// Whether the manager is showing an editor, and the row and question it holds.
    #[cfg(test)]
    #[must_use]
    pub fn snippet_panel(&self) -> &SnippetsPanel {
        &self.snippet_panel
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
        self.workspace.tab_anchors()
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

    /// Takes the keyboard to the terminal of one session.
    ///
    /// Focusing through the active window fails when there is none to borrow - a
    /// connection that finishes while the window is in the background, or while the
    /// window is busy with the event that started it - so the next draw also gives
    /// the terminal the keyboard with its own window.
    fn focus_pane(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_for(id) else {
            return;
        };
        let handle = pane.read(cx).focus_handle().clone();
        self.pane_focus_pending = Some(id);
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_view, window, _cx| window.focus(&handle));
        }
        cx.notify();
    }

    /// Puts the placeholders of the long-lived fields in the current language.
    fn relabel_fields(&self, cx: &mut Context<Self>) {
        let strings = self.strings();
        let labels = [
            (&self.shell_path, strings.shell_custom_placeholder),
            (&self.hosts_search, strings.hosts_search_placeholder),
            (&self.keys_search, strings.keys_search_placeholder),
            (&self.remote_fields.password, strings.auth_password),
            (&self.remote_fields.key_path, strings.auth_key),
            (&self.remote_fields.passphrase, strings.auth_key_passphrase),
            (&self.quick_address, strings.host_field_address),
            (&self.quick_port, strings.host_field_port),
            (&self.quick_user, strings.host_field_user),
            (&self.quick_password, strings.auth_password),
            (&self.quick_key, strings.auth_key),
            (&self.quick_passphrase, strings.auth_key_passphrase),
        ];
        for (field, label) in labels {
            field.update(cx, |field, cx| field.set_placeholder(label, cx));
        }
    }

    /// Puts the keyboard on what the page shows: the terminal on screen, or the page.
    /// Without this a control that disappeared would take every shortcut with it.
    fn reclaim_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let destination = self.workspace.destination();
        if self.shows_terminal(destination)
            && let Some(pane) = self.active_pane()
        {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
        } else {
            let handle = self.destination_focus(destination).clone();
            window.focus(&handle);
        }
        cx.notify();
    }

    /// The text of a session's target: the saved host's, or the unsaved one's that
    /// a quick connection made.
    fn target_of(&self, session: &Session) -> String {
        if let SessionKind::Remote { .. } = session.kind()
            && let Some((_, host)) = self.unsaved.iter().find(|(id, _)| *id == session.id())
        {
            return host.target();
        }
        session_target(&self.book, session)
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
        // A remote terminal asks the window to upload what was dropped or pasted.
        let session = tag.session;
        cx.subscribe(
            &pane,
            move |view, pane, request: &terminal::PaneRequest, cx| {
                view.answer_pane(session, &pane, request.clone(), cx);
            },
        )
        .detach();
        // The pane's toolbar names the shortcuts in force, so a rebound action is
        // described by the key the window really answers (R5).
        let shortcuts = terminal::TerminalShortcuts::of(&self.keymap);
        pane.update(cx, |pane, cx| pane.set_shortcuts(shortcuts, cx));
        self.panes.push(OpenPane { tag, pane });
        if focus {
            // A terminal that just opened is what the user asked for: show it, from
            // the hosts home or the settings alike.
            if self.workspace.destination() != Destination::LocalShells {
                self.workspace.select(Destination::LocalShells);
                self.workspace
                    .set_context(shellune_core::PanelSection::None);
            }
            self.active = Some(tag.session);
            self.session_terminal = true;
            self.showing_connection_form = false;
            self.focus_pane(tag.session, cx);
        }
        cx.notify();
    }

    /// Uploads what a remote terminal was given, then types where it went.
    ///
    /// The files go to the folder the file view shows, or, before it was opened,
    /// to where the server starts a file session (the user's home), which the
    /// shell reaches as `~`. The path is typed while the upload runs; the
    /// transfer strip shows its progress.
    fn answer_pane(
        &mut self,
        id: SessionId,
        pane: &Entity<TerminalPane>,
        request: terminal::PaneRequest,
        cx: &mut Context<Self>,
    ) {
        let locals: Vec<String> = match request {
            terminal::PaneRequest::UploadFiles(paths) => paths
                .iter()
                .filter(|path| path.is_file())
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            terminal::PaneRequest::PasteImage(image) => {
                let extension = image
                    .format
                    .mime_type()
                    .rsplit('/')
                    .next()
                    .map_or("png", |subtype| subtype.trim_end_matches("+xml"));
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_millis());
                let path = std::env::temp_dir().join(format!("shellune-paste-{stamp}.{extension}"));
                if let Err(error) = std::fs::write(&path, &image.bytes) {
                    let title = copy::ux().drop_upload_failed;
                    self.toast(Tone::Danger, title, Some(error.to_string()), cx);
                    return;
                }
                vec![path.to_string_lossy().into_owned()]
            }
        };
        if locals.is_empty() {
            self.toast(Tone::Warning, copy::ux().drop_folders_unsupported, None, cx);
            return;
        }
        let directory = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned);
        let mut typed = Vec::new();
        let protocol = self.session_protocol(id);
        for local in &locals {
            match self.transfers.upload_over(
                id,
                local,
                Some(directory.as_deref().unwrap_or(".")),
                protocol,
                false,
            ) {
                Ok(_) => {
                    let name = std::path::Path::new(local)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let remote = match &directory {
                        Some(directory) => shellune_core::join_path(directory, &name),
                        None => format!("~/{name}"),
                    };
                    // `~` must stay outside the quotes for the shell to expand it.
                    typed.push(match remote.strip_prefix("~/") {
                        Some(rest) => format!("~/{}", terminal::quote_posix_path(rest)),
                        None => terminal::quote_posix_path(&remote),
                    });
                }
                Err(problem) => {
                    let reason = transfer_problem_text(&problem, self.strings());
                    let title = copy::ux().drop_upload_failed;
                    self.toast(Tone::Danger, title, Some(format!("{local} · {reason}")), cx);
                }
            }
        }
        if !typed.is_empty() {
            let count = typed.len().to_string();
            let mut text = typed.join(" ");
            text.push(' ');
            pane.update(cx, |pane, cx| pane.type_text(&text, cx));
            self.watch_files(cx);
            let title = copy::fill(copy::ux().drop_uploading, &[("count", &count)]);
            let folder = directory.unwrap_or_else(|| "~".to_owned());
            self.toast(Tone::Accent, title, Some(folder), cx);
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
        self.workspace.layout_mut().focus_session(id);
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
        if self.workspace.destination() != Destination::LocalShells {
            self.workspace.select(Destination::LocalShells);
            self.workspace
                .set_context(shellune_core::PanelSection::None);
        }
        self.focus_pane(id, cx);
        // Choosing a tab is looking at it: whatever it was marked for has been seen.
        self.clear_viewed_attention(cx);
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
        if !self.workspace.layout_mut().select_index(index) {
            return;
        }
        self.show_layout_tab(window, cx);
    }

    /// Shows the tab `delta` places from the current one, wrapping at both ends.
    pub fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.workspace.layout().tabs().len();
        if count == 0 {
            return;
        }
        let current = self
            .workspace
            .layout()
            .active_tab_id()
            .and_then(|id| {
                self.workspace
                    .layout()
                    .tabs()
                    .iter()
                    .position(|tab| tab.id() == id)
            })
            .unwrap_or(0);
        // A tab that is still connecting has no terminal to show, so it is stepped over
        // instead of stopping the cycle on it. A pane that is only waiting is a stop:
        // it is on screen, and it has not been started.
        let mut next = current;
        for _ in 0..count {
            next = crate::app::palette::step(next, delta, count);
            if self.layout_tab_is_shown(next) {
                self.workspace.layout_mut().select_index(next);
                self.show_layout_tab(window, cx);
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
        self.stop_session_forwards(id);
        if let Some(mut link) = self.forward_carriers.remove(&id) {
            link.close();
        }
        self.forwards.detach(id);
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
            self.chain = None;
            self.showing_connection_form = false;
        }
        if self.reconnect == Some(id) {
            self.reconnect = None;
        }
        // A name dialog or a tab menu for a tab that no longer exists would describe
        // something gone; it closes with the tab it belonged to.
        self.forget_closed_tab_layers();
        // A prompt that was waiting in the closed terminal is gone with it; its note goes
        // too, rather than promising a confirmation nothing can give any more.
        self.settle_snippet_note_if_answered(cx);
        self.sync_layout_focus(cx);
        self.checkpoint_layout();
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Transfers (R10, R17)
    // -----------------------------------------------------------------------

    /// Asks for the local file to upload into a remote directory (R10).
    ///
    /// Two things are visible before anything is sent: the remote directory the file
    /// would land in - editable, because a server without SFTP has no listing to pick
    /// one from - and the protocol that would copy it, which starts at the host's own
    /// choice and can be changed for this one transfer.
    fn files_ask_upload(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let directory = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned)
            .unwrap_or_else(|| "~".to_owned());
        let protocol = self.session_protocol(id);
        let placeholder = self.strings().transfers_upload_placeholder;
        let field = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_browse(Browse::File, cx);
            field
        });
        let remote_placeholder = self.strings().transfers_remote_directory;
        let remote = cx.new(|cx| TextField::with_text(remote_placeholder, &directory, cx));
        self.open_files_dialog_with(
            FilesDialog::Upload {
                session: id,
                source: FileSource::Remote,
                directory,
                text: String::new(),
                protocol,
                recursive: false,
                problem: None,
            },
            field,
            Some(remote),
            cx,
        );
    }

    /// Asks which remote path is downloaded, and where it goes (R10).
    ///
    /// The remote path is editable for the same reason an upload's directory is, and the
    /// folder starts at this account's home directory: a real place on this machine that
    /// the user can correct, instead of an empty field with no hint.
    ///
    /// A row asks about the entry it names. The toolbar and the failure card ask about
    /// nothing in particular - a server without an SFTP subsystem has no listing to name
    /// an entry from - so the remote path is typed, starting at that account's home, and
    /// the download is the same task either way.
    fn files_ask_download(&mut self, id: SessionId, name: Option<&str>, cx: &mut Context<Self>) {
        let directory = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned);
        let remote = match (directory.as_deref(), name) {
            (Some(directory), Some(name)) => join_path(directory, name),
            (Some(directory), None) => directory.to_owned(),
            // No listing means no directory to start from; `~` is where a session
            // starts, and it is a path the user can see and correct before confirming.
            (None, _) => "~".to_owned(),
        };
        let protocol = self.session_protocol(id);
        let placeholder = self.strings().transfers_download_placeholder;
        let suggested = default_download_directory();
        let field = cx.new(|cx| {
            let mut field = TextField::with_text(placeholder, &suggested, cx);
            field.set_browse(Browse::Directory, cx);
            field
        });
        let remote_placeholder = self.strings().transfers_remote_path;
        let remote_field = cx.new(|cx| TextField::with_text(remote_placeholder, &remote, cx));
        self.open_files_dialog_with(
            FilesDialog::Download {
                session: id,
                source: FileSource::Remote,
                remote,
                text: suggested,
                protocol,
                recursive: false,
                problem: None,
            },
            field,
            Some(remote_field),
            cx,
        );
    }

    /// Starts the upload the question described, or says why it cannot start.
    ///
    /// Two ways reach this: the question's confirm button, whose three choices it reads,
    /// and a row button or a drop, which has no question and therefore uses the directory
    /// on screen and the host's own protocol.
    fn start_upload(&mut self, id: SessionId, local: &str, cx: &mut Context<Self>) {
        let (directory, protocol, recursive) =
            self.transfer_question_for(id).unwrap_or_else(|| {
                (
                    self.on_screen_remote_directory(id),
                    self.session_protocol(id),
                    false,
                )
            });
        let started =
            self.transfers
                .upload_over(id, local, directory.as_deref(), protocol, recursive);
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
        let (_, protocol, recursive) = self
            .transfer_question_for(id)
            .unwrap_or_else(|| (None, self.session_protocol(id), false));
        let started = self
            .transfers
            .download_over(id, remote, folder, protocol, recursive);
        self.finish_transfer_question(id, started, cx);
    }

    /// The remote directory the file view of this session is showing.
    fn on_screen_remote_directory(&self, id: SessionId) -> Option<String> {
        self.files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned)
    }

    /// The remote path, protocol and recursive choice the open question carries.
    ///
    /// `None` means no transfer question is open for this session. The question is the
    /// one place these three were chosen, so a task started from it is built from it and
    /// not from a second opinion about the same facts.
    fn transfer_question_for(
        &self,
        id: SessionId,
    ) -> Option<(Option<String>, TransferProtocol, bool)> {
        let dialog = self
            .files_dialog
            .as_ref()
            .filter(|dialog| dialog.session() == id)?;
        Some((
            dialog.remote().map(str::to_owned),
            dialog.protocol()?,
            dialog.recursive(),
        ))
    }

    /// The protocol this session starts a transfer with (R10).
    ///
    /// It is the choice saved on the host the session was opened from, so a server
    /// without an SFTP subsystem does not need the protocol picked again for every file.
    /// A session that was opened without a saved host starts from the SFTP default.
    fn session_protocol(&self, id: SessionId) -> TransferProtocol {
        self.workspace
            .session(id)
            .and_then(|session| match session.kind() {
                SessionKind::Remote { host } => {
                    self.book.host(*host).map(HostConfig::transfer_protocol)
                }
                SessionKind::Local { .. } => None,
            })
            .unwrap_or_default()
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
                self.clear_files_fields();
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
                    // A directory named without the recursive choice is a mistake in what
                    // was typed, so the field says so; a connection that could not take
                    // the command is not, and is reported beside the list instead.
                    TransferProblem::DirectoryNeedsRecursion => Some(PathProblem::NeedsRecursion),
                    // A directory over a protocol that copies one file has its own next
                    // step - the other protocol - so the question names it instead of
                    // calling the directory a path without a name.
                    TransferProblem::DirectoryNeedsScp => Some(PathProblem::DirectoryNeedsScp),
                };
                match (field_problem, self.files_dialog.as_mut()) {
                    (Some(field_problem), Some(dialog)) => dialog.fail_path(field_problem),
                    _ => {
                        self.files_dialog = None;
                        self.clear_files_fields();
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

    /// Starts a new task for a transfer that failed.
    ///
    /// A checkpoint, when the failed task has one, is carried for the worker to
    /// re-check. The new task stays queued until that check reports real bytes, and
    /// the failed attempt stays in the list beside it.
    fn transfer_retry(&mut self, id: shellune_core::TransferId, cx: &mut Context<Self>) {
        self.transfers.retry(id);
        self.watch_files(cx);
        self.sync_transfer_section();
        cx.notify();
    }

    /// Starts again from the first byte, ignoring any checkpoint.
    fn transfer_restart(&mut self, id: shellune_core::TransferId, cx: &mut Context<Self>) {
        self.transfers.restart(id);
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
    /// Whether closing this session would stop a transfer or a forward (R9, R11).
    pub(crate) fn close_needs_confirmation(&self, id: SessionId) -> bool {
        self.transfers.active_count(id) > 0 || !self.forwards.active_on(id).is_empty()
    }

    pub fn request_close_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.close_needs_confirmation(id) {
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

    /// The question shown before a session with active transfers is closed (R9).
    fn close_transfer_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let id = self.closing?;
        let active: Vec<Transfer> = self
            .transfers
            .for_session(id)
            .filter(|task| task.is_active())
            .cloned()
            .collect();
        let rules = self.forwards.active_on(id);
        if active.is_empty() && rules.is_empty() {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let target = self
            .workspace
            .session(id)
            .map_or_else(String::new, |session| session.title().to_owned());
        let heading = if active.is_empty() {
            format!("{}: {target}", strings.forward_close_rules)
        } else {
            format!(
                "{}: {target} · {}",
                strings.transfers_close_question,
                active.len()
            )
        };
        let mut question = components::banner(Tone::Danger, metrics)
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .mx(metrics.scale(spacing::SNUG))
            .mt(metrics.scale(spacing::SNUG))
            .w_auto()
            .debug_selector(|| "session.close.question".to_owned())
            .child(
                div()
                    .min_w(px(0.0))
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "session.close.question.text".to_owned())
                    .child(SharedString::from(heading)),
            );
        question = self.interrupted_task_lines(question, "session.close.task", &active, metrics);
        if !rules.is_empty() {
            if !active.is_empty() {
                question = question.child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(strings.forward_close_rules),
                );
            }
            question =
                self.interrupted_forward_lines(question, "session.close.forward", &rules, metrics);
        }
        Some(
            question.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
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
            ),
        )
    }

    /// Refuses to close the window while transfers are still running (R9).
    ///
    /// Returning false keeps the window, and the question lists every task that would
    /// stop. Cancelling that question leaves them running.
    fn refuse_app_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.app_close_confirmed || (!self.transfers.any_active() && !self.forwards.any_active())
        {
            return true;
        }
        self.closing_app = true;
        cx.notify();
        false
    }

    fn cancel_close_app(&mut self, cx: &mut Context<Self>) {
        self.closing_app = false;
        cx.notify();
    }

    fn confirm_close_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.app_close_confirmed = true;
        self.closing_app = false;
        self.close_all_sessions(cx);
        window.remove_window();
    }

    /// The question shown before the application closes with transfers still running.
    fn close_app_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.closing_app {
            return None;
        }
        let active: Vec<Transfer> = self
            .transfers
            .transfers()
            .iter()
            .filter(|task| task.is_active())
            .cloned()
            .collect();
        let rules = self.forwards.active_rules();
        if active.is_empty() && rules.is_empty() {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let heading = if active.is_empty() {
            strings.forward_close_app
        } else {
            strings.transfers_close_app_question
        };
        let mut question = components::banner(Tone::Danger, metrics)
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .mx(metrics.scale(spacing::SNUG))
            .mt(metrics.scale(spacing::SNUG))
            .w_auto()
            .debug_selector(|| "app.close.question".to_owned())
            .child(
                div()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "app.close.question.text".to_owned())
                    .child(SharedString::from(heading)),
            );
        question = self.interrupted_task_lines(question, "app.close.task", &active, metrics);
        if !rules.is_empty() && !active.is_empty() {
            question = question.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(strings.forward_close_app),
            );
        }
        question = self.interrupted_forward_lines(question, "app.close.forward", &rules, metrics);
        let confirm = components::compact_button(
            strings.transfers_close_app_confirm,
            Variant::Danger,
            metrics,
        )
        .id("app.close.confirm")
        .debug_selector(|| "app.close.confirm".to_owned())
        .on_click(cx.listener(|this, _event: &gpui::ClickEvent, window, cx| {
            this.confirm_close_app(window, cx);
        }));
        Some(
            question.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .child(self.text_button(
                        SharedString::from("app.close.cancel"),
                        strings.transfers_close_app_cancel,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.cancel_close_app(cx),
                    ))
                    .child(confirm),
            ),
        )
    }

    /// Direction, host, source and target of a task a close would interrupt.
    fn interrupted_forward_lines(
        &self,
        mut question: Div,
        prefix: &'static str,
        rules: &[ForwardRule],
        metrics: Metrics,
    ) -> Div {
        for (index, rule) in rules.iter().enumerate() {
            let target = rule
                .target()
                .map(|endpoint| endpoint.to_string())
                .unwrap_or_else(|| "SOCKS".to_owned());
            question = question.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector({
                        let prefix = prefix.to_owned();
                        move || format!("{prefix}.{index}")
                    })
                    .child(SharedString::from(format!(
                        "{} · {} → {target}",
                        rule.kind().label(),
                        rule.listen()
                    ))),
            );
        }
        question
    }

    fn interrupt_line(&self, task: &Transfer) -> String {
        format!(
            "{} · {} · {} → {}",
            self.direction_label(task.direction()),
            self.transfer_host_label(task),
            task.source(),
            task.destination()
        )
    }

    /// One line per task a close would interrupt: direction, host, source and target.
    fn interrupted_task_lines(
        &self,
        mut question: Div,
        prefix: &'static str,
        tasks: &[Transfer],
        metrics: Metrics,
    ) -> Div {
        for (index, task) in tasks.iter().enumerate() {
            let line = self.interrupt_line(task);
            question = question.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector({
                        let prefix = prefix.to_owned();
                        move || format!("{prefix}.{index}")
                    })
                    .child(SharedString::from(line)),
            );
        }
        question
    }

    /// Closes every session, which is what leaving the application does (R7).
    ///
    /// Each source is shut down through its own session, so the processes and the
    /// connection threads end with the window instead of outliving it.
    pub fn close_all_sessions(&mut self, cx: &mut Context<Self>) {
        // Shutdown must not overwrite the saved tree with the empty one this close
        // produces. The next launch restores that tree as pending panes.
        self.layout_persist = false;
        let mut ids = self.open_sessions();
        for id in self.forwards.carrier_sessions() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in self.forward_carriers.keys().copied() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in ids {
            self.close_session(id, cx);
        }
        let pending: Vec<SessionId> = self.attempts.iter().map(|attempt| attempt.id).collect();
        for id in pending {
            self.discard_attempt(id);
            self.workspace.session_closed(id);
        }
        self.remote = None;
        self.chain = None;
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
                self.park_focused_form();
                self.discard_attempt(id);
                self.reconnect = Some(id);
                self.form_session = Some(id);
                self.chain = Some(ChainDraft::prepare(self.book.hosts(), &host));
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
    #[cfg(test)]
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

    /// Selects a destination, moving keyboard focus with it.
    ///
    /// Returning to the workspace gives the keyboard back to the session that was
    /// on screen. A host list that is still loading does not get in the way: the
    /// destination changes either way.
    fn select(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        // A capture layer that is still open belongs to the page that opened it; leaving
        // it behind would keep taking every key.
        if self.key_capture.is_some() {
            self.keys_capture_cancel(cx);
        }
        // Transfers and forwards are tools over the page, not pages; the old session
        // list is the terminal tabs.
        let destination = match destination {
            Destination::Transfers => {
                self.open_tool(shell::Tool::Transfers, cx);
                return;
            }
            Destination::Forwards => {
                self.forwards
                    .set_scope(crate::app::forwarding::ForwardScope::All);
                self.sync_forward_section();
                self.open_tool(shell::Tool::Forwards, cx);
                return;
            }
            Destination::Sessions => Destination::LocalShells,
            other => other,
        };
        self.shell_menu_open = false;
        if destination == Destination::LocalShells {
            self.showing_connection_form = false;
        }
        self.workspace.select(destination);
        self.workspace.set_context(match destination {
            Destination::Hosts => shellune_core::PanelSection::Hosts,
            Destination::Transfers => shellune_core::PanelSection::Transfers,
            Destination::Forwards => shellune_core::PanelSection::Forwards,
            _ => shellune_core::PanelSection::None,
        });
        if self.shows_terminal(destination)
            && let Some(pane) = self.active_pane()
        {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
            // The terminal is on screen again, so its tab's background marks are seen.
            self.clear_viewed_attention(cx);
            cx.notify();
            return;
        }
        let handle = self.destination_focus(destination).clone();
        window.focus(&handle);
        cx.notify();
    }

    /// Opens the default shell in a new tab, from the header, the tab strip or the
    /// shortcut.
    fn new_local_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell_menu_open = false;
        // A new shell is its own tab. A split that was armed and not used must not
        // swallow it.
        self.workspace.clear_armed();
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

    /// Redraws the settings page as the shortcut search is typed into.
    ///
    /// The filter is read from the field while the page draws, so nothing has to be
    /// copied; the subscription exists only to make the page draw again.
    fn bind_keys_search(&mut self, cx: &mut Context<Self>) {
        let search = self.keys_search.clone();
        cx.observe(&search, |_view, _field, cx| {
            cx.notify();
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
                self.recheck_open_hosts();
            }
            Err(error) => {
                // A file that cannot be read is not an empty list: the section keeps
                // the reason the storage layer produced and holds no records, so no
                // host is shown that the file does not really contain.
                self.book = HostBook::new();
                self.host_fields = None;
                self.workspace
                    .fail_hosts(Failure::new(error.to_string(), Recovery::Retry));
                self.recheck_open_hosts();
            }
        }
        cx.notify();
    }

    /// Marks only the remote panes whose host is no longer saved.
    ///
    /// Clearing a mark does not connect the pane.
    fn recheck_open_hosts(&mut self) {
        let ids: Vec<HostId> = self.book.hosts().iter().map(HostConfig::id).collect();
        self.workspace.recheck_layout_hosts(|id| ids.contains(&id));
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
        let proxy = cx.new(|cx| {
            TextField::with_text(
                strings.host_proxy_placeholder,
                draft.text(HostField::ProxyCommand),
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
            proxy,
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
        self.restore_focus_pending = true;
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
                self.restore_focus_pending = true;
                self.hosts.set_message(message.to_owned(), false);
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
                self.recheck_open_hosts();
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
        let withheld = unimported_hosts(&preview, &plan);
        let (message, incomplete) =
            import_message(self.strings(), imported, review, skipped, &withheld);

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
                self.recheck_open_hosts();
                if let Err(error) = self.write_import_forwards(&plan) {
                    // The host file changed. The preview stays, and the message does
                    // not describe that partial write as a complete import.
                    self.import.set_apply_error(format!(
                        "{} {error}",
                        self.strings().import_forwards_failed
                    ));
                    cx.notify();
                    return;
                }
                self.reload_forwards();
                // A finished import still names every host and rule that did not land.
                // The flag stays a failure while any of those remain, so the line is
                // not the success text of a complete import.
                self.hosts.set_message(message, incomplete);
                self.import.close();
                self.import_path = None;
            }
            // The file did not change, so the preview stays: the same hosts can be
            // applied again once the write is possible. Forwarding rules are not
            // written when the hosts were not.
            Err(error) => self.import.set_apply_error(error.to_string()),
        }
        cx.notify();
    }

    /// Writes the forwarding rules the plan listed, after the hosts are on disk.
    ///
    /// An empty plan does not create a forwarding file. A failure leaves the preview
    /// in place; the caller has already adopted the host book.
    fn write_import_forwards(&self, plan: &shellune_storage::ImportPlan) -> Result<(), String> {
        if plan.forwards().is_empty() {
            return Ok(());
        }
        let store = ForwardStore::at(self.store.root());
        let mut book = store.load().map_err(|error| error.to_string())?;
        let mut added = false;
        for planned in plan.forwards() {
            let host = self
                .book
                .hosts()
                .iter()
                .find(|host| host.name().eq_ignore_ascii_case(planned.alias()))
                .ok_or_else(|| format!("forwarding for {} has no saved host", planned.alias()))?;
            let rule = planned
                .forward()
                .rule(host.id())
                .map_err(|error| error.to_string())?;
            let already = book.rules().iter().any(|saved| {
                saved.host() == host.id()
                    && saved.kind() == rule.kind()
                    && saved.listen() == rule.listen()
                    && saved.target() == rule.target()
            });
            if already {
                continue;
            }
            book.insert(rule).map_err(|error| error.to_string())?;
            added = true;
        }
        if added {
            store.save(&book).map_err(|error| error.to_string())?;
        }
        Ok(())
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
    fn link_mut(&mut self, id: SessionId) -> Option<&mut AttemptLink> {
        self.attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .and_then(|attempt| attempt.link.as_mut())
    }

    /// Takes the transport of one attempt and drops that attempt.
    ///
    /// The focused form stays in [`Self::remote`]. A parked form is dropped with the
    /// attempt: the caller already copied anything it still needs, such as the host.
    /// A failure uses [`Self::close_attempt_transport`] instead, so the parked form
    /// can stay and show why that one attempt ended.
    fn take_transport(&mut self, id: SessionId) -> Option<AttemptLink> {
        let index = self.attempt_index(id)?;
        let mut attempt = self.attempts.remove(index);
        if self.form_session == Some(id) && self.remote.is_none() {
            self.remote = attempt.panel.take();
            self.chain = attempt.chain.take();
        }
        attempt.link.take()
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
            if let Some(mut link) = self.take_transport(id) {
                link.close();
            }
        } else if let Some(index) = self.attempt_index(id) {
            if let Some(mut link) = self.attempts[index].link.take() {
                link.close();
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
        self.attempts[index].chain = self.chain.take();
    }

    /// Parks the form on screen, including one that has not dialled yet.
    ///
    /// An idle form has no session until something else needs the screen. Registering
    /// one then keeps its typed secrets on its own attempt instead of dropping them
    /// when the next form opens.
    ///
    /// A form can already own a session and still have no attempt: dialling was refused
    /// before a transport existed, or the attempt was removed when that transport
    /// ended. That form is hung on the session it already has. A new session here would
    /// leave the one on screen to be replaced.
    fn park_focused_form(&mut self) {
        // A leftover split or adopt belongs to the action that armed it. Parking a
        // form must not bind that form's session into the waiting pane.
        self.workspace.clear_armed();
        if let Some(id) = self.form_session {
            if self.attempt_index(id).is_some() {
                self.stash_focused_panel();
            } else {
                self.park_session_form(id);
            }
            return;
        }
        let Some(panel) = self.remote.take() else {
            self.chain = None;
            return;
        };
        let target = self
            .chain
            .as_ref()
            .and_then(ChainDraft::target_host)
            .cloned()
            .unwrap_or_else(|| panel.host().clone());
        let session = self.workspace.register_remote_session(&target);
        let id = session.id();
        let generation = self
            .workspace
            .session(id)
            .map(Session::generation)
            .unwrap_or(0);
        let ephemeral = self
            .pending_ephemeral
            .take()
            .is_some_and(|host| host.id() == target.id());
        self.attempts.push(Attempt {
            id,
            generation,
            link: None,
            watching: false,
            panel: Some(panel),
            chain: self.chain.take(),
            pending_trust: None,
            asking_hop: None,
            target,
            ephemeral,
            forward_only: false,
            pending_rules: Vec::new(),
        });
        self.checkpoint_layout();
    }

    /// Hangs the focused form on a session that does not have an attempt yet.
    ///
    /// The session id is the one the form already shows. Nothing new is registered.
    fn park_session_form(&mut self, id: SessionId) {
        let Some(panel) = self.remote.take() else {
            self.chain = None;
            return;
        };
        let target = self
            .chain
            .as_ref()
            .and_then(ChainDraft::target_host)
            .cloned()
            .unwrap_or_else(|| panel.host().clone());
        let generation = self
            .workspace
            .session(id)
            .map(Session::generation)
            .unwrap_or(0);
        let ephemeral = self
            .pending_ephemeral
            .take()
            .is_some_and(|host| host.id() == target.id());
        self.attempts.push(Attempt {
            id,
            generation,
            link: None,
            watching: false,
            panel: Some(panel),
            chain: self.chain.take(),
            pending_trust: None,
            asking_hop: None,
            target,
            ephemeral,
            forward_only: false,
            pending_rules: Vec::new(),
        });
    }

    /// Shows one attempt's own form without touching any other attempt.
    fn focus_attempt(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.form_session != Some(id) {
            self.park_focused_form();
            let Some(index) = self.attempt_index(id) else {
                cx.notify();
                return;
            };
            self.remote = self.attempts[index].panel.take();
            self.chain = self.attempts[index].chain.take();
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
    /// reason, and what was typed is kept. The chosen method is the only one whose
    /// secret is collected.
    pub(crate) fn prepare_quick_connection(&mut self, cx: &mut Context<Self>) -> bool {
        let strings = self.strings();
        let address = self.quick_address.read(cx).text().trim().to_owned();
        let port_text = self.quick_port.read(cx).text().trim().to_owned();
        let user = self.quick_user.read(cx).text().trim().to_owned();
        let password = self.quick_password.read(cx).text().to_owned();
        let key_path = self.quick_key.read(cx).text().trim().to_owned();
        let passphrase = self.quick_passphrase.read(cx).text().to_owned();
        let port = match port_text.parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => {
                self.refuse_quick(quick_port_reason(&port_text, strings), cx);
                return false;
            }
        };
        let name = if user.is_empty() {
            address.clone()
        } else {
            format!("{user}@{address}")
        };
        let mut host = match HostConfig::new(name, address, port, user) {
            Ok(host) => host,
            Err(error) => {
                self.refuse_quick(quick_host_reason(&error, strings), cx);
                return false;
            }
        };
        let method = self.quick_method;
        if let Some(reason) = quick_auth_gap(method, &password, &key_path, &passphrase, strings) {
            self.refuse_quick(reason, cx);
            return false;
        }
        apply_quick_auth(&mut host, method, &key_path);
        self.park_focused_form();
        self.reconnect = None;
        self.form_session = None;
        let mut panel = RemotePanel::new(&host);
        match method {
            MethodChoice::Password => panel.set_password(password),
            MethodChoice::KeyFile => panel.set_key_path(key_path),
            MethodChoice::EncryptedKeyFile => {
                panel.set_key_path(key_path);
                panel.set_passphrase(passphrase);
            }
            MethodChoice::Agent => {}
        }
        self.chain = Some(ChainDraft::single(host.clone()));
        self.pending_ephemeral = Some(host);
        self.remote = Some(panel);
        self.showing_connection_form = true;
        self.quick_error = None;
        self.sync_remote_fields(cx);
        cx.notify();
        // The form has done its job: the connection it describes is prepared. Its
        // fields leave the screen, so the keyboard goes back where it came from.
        self.quick_open = false;
        self.restore_focus_pending = true;
        true
    }

    /// Keeps a refusal on the quick-connection form.
    fn refuse_quick(&mut self, reason: String, cx: &mut Context<Self>) {
        self.quick_error = Some(reason);
        cx.notify();
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
        let strings = self.strings();
        let mut next = self.book.clone();
        if let Err(error) = next.insert(host.clone()) {
            let (reason, next_step) = quick_book_failure(&error, strings);
            self.quick_save_notice = Some(QuickSaveNotice {
                failed: true,
                reason,
                next: next_step,
            });
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
                self.quick_save_notice = Some(QuickSaveNotice {
                    failed: false,
                    reason: strings.quick_saved.to_owned(),
                    next: strings.quick_saved_next.to_owned(),
                });
            }
            Err(error) => {
                let (reason, next_step) = quick_storage_failure(&error, strings);
                self.quick_save_notice = Some(QuickSaveNotice {
                    failed: true,
                    reason,
                    next: next_step,
                });
            }
        }
        cx.notify();
    }

    /// Opens the connection form for one saved host.
    ///
    /// An attempt that is already running is parked with its own form. It is not
    /// cancelled, and its session stays. The new form asks only for what this host
    /// still needs.
    pub(crate) fn open_remote(&mut self, host: &HostConfig, cx: &mut Context<Self>) {
        self.forward_only_dial = false;
        self.pending_forward_rules.clear();
        self.park_focused_form();
        self.open_remote_after_park(host, cx);
    }

    /// Opens a host form after any focused form has been parked.
    ///
    /// Does not clear an adopt aimed at a waiting pane. [`Self::open_remote`] clears
    /// that aim by parking first; a pane's own Connect button arms the adopt after
    /// the park, then calls this.
    pub(super) fn open_remote_after_park(&mut self, host: &HostConfig, cx: &mut Context<Self>) {
        self.reconnect = None;
        self.form_session = None;
        self.pending_ephemeral = None;
        self.chain = Some(ChainDraft::prepare(self.book.hosts(), host));
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
        if self.chain.is_none() {
            self.chain = Some(ChainDraft::prepare(self.book.hosts(), host));
        }
        self.finish_chain(false, cx);
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
        self.finish_chain(true, cx);
    }

    /// Collects credentials hop by hop and dials when every hop has them.
    ///
    /// `take_panel` is set when the user pressed connect: the form's fields are the
    /// current hop. Opening a host does not consume an empty field; it asks for the
    /// first hop that is still missing something.
    fn finish_chain(&mut self, take_panel: bool, cx: &mut Context<Self>) {
        if self
            .remote
            .as_ref()
            .is_some_and(|panel| panel.status().is_busy())
        {
            return;
        }
        if self.chain.is_none()
            && let Some(host) = self.remote.as_ref().map(|panel| panel.host().clone())
        {
            let draft = if self
                .pending_ephemeral
                .as_ref()
                .is_some_and(|pending| pending.id() == host.id())
            {
                ChainDraft::single(host)
            } else {
                ChainDraft::prepare(self.book.hosts(), &host)
            };
            self.chain = Some(draft);
        }
        if let Some(reason) = self
            .chain
            .as_ref()
            .and_then(|draft| draft.fault().map(str::to_owned))
        {
            self.fail_before_dial(reason, cx);
            return;
        }
        if take_panel && !self.take_panel_auth(cx) {
            return;
        }
        self.fill_or_dial(cx);
    }

    /// Moves the form's credentials onto the hop it is showing.
    ///
    /// Returns false when the form refused, which leaves the chain undialled.
    fn take_panel_auth(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(host) = self.remote.as_ref().map(|panel| panel.host().clone()) else {
            return false;
        };
        if self
            .chain
            .as_ref()
            .is_some_and(|draft| draft.has_auth(host.id()))
        {
            return true;
        }
        let strings = self.workspace.strings();
        let auth = match self.remote.as_mut().map(|panel| panel.take_auth(strings)) {
            Some(Ok(auth)) => auth,
            Some(Err(reason)) => {
                if let Some(panel) = self.remote.as_mut() {
                    panel.set_form_error(reason);
                }
                cx.notify();
                return false;
            }
            None => return false,
        };
        let known = remote::known_host_key(&self.trust_book, &host);
        if let Some(draft) = self.chain.as_mut() {
            draft.push_auth(shellune_ssh::HopAuth::new(host.id(), auth, known));
        }
        true
    }

    /// Fills hops that already have credentials, then dials or asks for the next one.
    fn fill_or_dial(&mut self, cx: &mut Context<Self>) {
        loop {
            let next = self.chain.as_ref().and_then(ChainDraft::next_needed);
            let Some(hop_id) = next else {
                self.dial_chain(cx);
                return;
            };
            let host = self
                .chain
                .as_ref()
                .and_then(|draft| draft.host(hop_id))
                .cloned();
            let Some(host) = host else {
                self.fail_before_dial(
                    "the jump chain names a host that is not saved. Nothing was connected and no command was run.".to_owned(),
                    cx,
                );
                return;
            };
            match self.saved_secret(&host) {
                Err(reason) => {
                    self.show_hop(host, cx);
                    if let Some(panel) = self.remote.as_mut() {
                        panel.set_form_error(reason);
                    }
                    cx.notify();
                    return;
                }
                Ok(secret) => {
                    if self.absorb_saved(&host, secret.as_ref(), cx) {
                        continue;
                    }
                }
            }
            self.show_hop(host, cx);
            cx.notify();
            return;
        }
    }

    /// The remembered secret, or an error when the store cannot be read.
    fn saved_secret(
        &self,
        host: &HostConfig,
    ) -> Result<Option<shellune_core::SecretString>, String> {
        if !host.remembers_secret() {
            return Ok(None);
        }
        self.secrets
            .load(host.id())
            .map_err(|error| format!("{}: {error}", self.strings().remote_saved_secret_failed))
    }

    /// Records a complete saved sign-in for one hop.
    ///
    /// The hop on screen is copied into the fields first, so a one-click connection
    /// still shows the password it used. An earlier hop is recorded without replacing
    /// the form.
    fn absorb_saved(
        &mut self,
        host: &HostConfig,
        secret: Option<&shellune_core::SecretString>,
        cx: &mut Context<Self>,
    ) -> bool {
        if remote::stored_auth(host, secret).is_none() {
            return false;
        }
        let shown = self
            .remote
            .as_ref()
            .is_some_and(|panel| panel.host().id() == host.id());
        if shown {
            if let Some(panel) = self.remote.as_mut() {
                match host.auth() {
                    Some(shellune_core::AuthMethodKind::Password) => {
                        if let Some(secret) = secret {
                            panel.set_password(secret.expose());
                        }
                    }
                    Some(shellune_core::AuthMethodKind::EncryptedKeyFile) => {
                        if let Some(secret) = secret {
                            panel.set_passphrase(secret.expose());
                        }
                    }
                    _ => {}
                }
            }
            self.sync_remote_fields(cx);
        }
        let known = remote::known_host_key(&self.trust_book, host);
        let auth = if shown {
            let strings = self.workspace.strings();
            match self.remote.as_mut().map(|panel| panel.take_auth(strings)) {
                Some(Ok(auth)) => auth,
                _ => return false,
            }
        } else {
            match remote::stored_auth(host, secret) {
                Some(auth) => auth,
                None => return false,
            }
        };
        if let Some(draft) = self.chain.as_mut() {
            draft.push_auth(shellune_ssh::HopAuth::new(host.id(), auth, known));
        }
        true
    }

    /// Shows the form for one hop and no other hop's fields.
    fn show_hop(&mut self, host: HostConfig, cx: &mut Context<Self>) {
        let multi = self
            .chain
            .as_ref()
            .is_some_and(|draft| draft.order().len() > 1);
        let label = multi.then(|| format!("{} {}", self.strings().remote_asking_hop, host.name()));
        let same = self
            .remote
            .as_ref()
            .is_some_and(|panel| panel.host().id() == host.id());
        if !same {
            self.remote = Some(RemotePanel::new(&host));
            self.showing_connection_form = true;
            self.sync_remote_fields(cx);
        }
        if let Some(label) = label
            && let Some(panel) = self.remote.as_mut()
        {
            panel.set_hop_label(label);
        }
    }

    /// Refuses to dial and keeps the form. The host record is not changed.
    fn fail_before_dial(&mut self, reason: String, cx: &mut Context<Self>) {
        let host = self
            .chain
            .as_ref()
            .and_then(ChainDraft::target_host)
            .cloned()
            .or_else(|| self.remote.as_ref().map(|panel| panel.host().clone()));
        let Some(host) = host else {
            return;
        };
        let id = if let Some(id) = self
            .form_session
            .filter(|id| self.workspace.session(*id).is_some())
        {
            id
        } else if let Some(id) = self.reconnect.take() {
            id
        } else {
            let id = self.workspace.register_remote_session(&host).id();
            self.form_session = Some(id);
            id
        };
        self.form_session = Some(id);
        self.active = Some(id);
        if let Some(panel) = self.remote.as_mut() {
            panel.fail(RemoteFailure::new(
                reason.clone(),
                "Fix the jump chain or the ProxyCommand, then connect again.",
                true,
            ));
        }
        self.workspace
            .session_failed(id, SessionFailure::ConnectionFailed { reason });
        self.checkpoint_layout();
        cx.notify();
    }

    /// Dials the chain. A ProxyCommand stays unstarted until the user confirms it.
    fn dial_chain(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.reconnect
            && !self.workspace.session_reconnecting(id)
        {
            return;
        }
        let Some(draft) = self.chain.take() else {
            return;
        };
        let Some(target_host) = draft.target_host().cloned() else {
            self.chain = Some(draft);
            return;
        };
        let Some(mut request) = draft.into_request(PtySize::new(100, 30)) else {
            return;
        };
        let Some(id) = self.session_for_form(&target_host) else {
            return;
        };
        let forward_only = self.forward_only_dial;
        self.forward_only_dial = false;
        let pending_rules = if forward_only {
            request = request.without_shell();
            std::mem::take(&mut self.pending_forward_rules)
        } else {
            Vec::new()
        };
        if let Some(panel) = self.remote.as_mut() {
            panel.set_status(RemoteStatus::Connecting);
        }
        let ephemeral = self
            .pending_ephemeral
            .take()
            .is_some_and(|pending| pending.id() == target_host.id());
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
            link: Some(AttemptLink::Chain(shellune_ssh::ChainSession::connect(
                request,
            ))),
            watching: false,
            panel: None,
            chain: None,
            pending_trust: None,
            asking_hop: None,
            target: target_host,
            ephemeral,
            forward_only,
            pending_rules,
        });
        self.watch_attempt(id, cx);
        cx.notify();
    }

    /// The session this dial belongs to: a reconnect, a parked form, or a new record.
    fn session_for_form(&mut self, host: &HostConfig) -> Option<SessionId> {
        if let Some(id) = self.reconnect.take() {
            if !self.workspace.session_reconnecting(id) {
                return None;
            }
            return Some(id);
        }
        if let Some(id) = self
            .form_session
            .filter(|id| self.attempt_index(*id).is_some())
        {
            return Some(id);
        }
        let id = self.workspace.register_remote_session(host).id();
        self.checkpoint_layout();
        Some(id)
    }

    /// Polls one attempt until it becomes a terminal or fails.
    ///
    /// Each attempt has its own loop. One that ends does not stop the others.
    fn watch_attempt(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(attempt) = self.attempts.iter_mut().find(|attempt| attempt.id == id) else {
            return;
        };
        if attempt.watching || attempt.link.is_none() {
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
        let events = match self.attempts[index].link.as_mut() {
            Some(link) => link.drain(),
            None => return false,
        };
        let mut pending: Vec<SourceEvent> = Vec::new();
        let mut prompt: Option<(Option<HostId>, shellune_ssh::HostKeyPrompt)> = None;
        let mut proxy: Option<(HostId, String, String)> = None;
        let mut ready = Vec::new();
        let mut connected = false;
        let mut failed_remote = None;
        let mut failed_chain = None;
        for event in events {
            match event {
                LinkEvent::HostKey {
                    host_id,
                    prompt: asked,
                } => {
                    // Once the terminal is live the identity is settled, so a prompt
                    // arriving behind it is not a question the user can still answer.
                    if !connected {
                        prompt = Some((host_id, asked));
                    }
                }
                LinkEvent::Proxy {
                    host_id,
                    label,
                    command,
                } => {
                    if !connected {
                        proxy = Some((host_id, label, command));
                    }
                }
                LinkEvent::HopReady(hop) => ready.push(hop),
                LinkEvent::Connected => connected = true,
                LinkEvent::FailedRemote(error) => {
                    if connected {
                        pending.push(SourceEvent::Lost(error.to_string()));
                        continue;
                    }
                    failed_remote = Some(error);
                }
                LinkEvent::FailedChain(failure) => {
                    if connected {
                        // The chain's display names the hop. session_failure would
                        // turn a disconnect into a failure whose host is "the server".
                        pending.push(SourceEvent::Lost(failure.to_string()));
                        continue;
                    }
                    failed_chain = Some(failure);
                }
                LinkEvent::Data(bytes) => {
                    if connected {
                        pending.push(SourceEvent::Data(bytes));
                    }
                }
                LinkEvent::Exited { code } => {
                    if connected {
                        pending.push(SourceEvent::Exited(match code {
                            None | Some(0) => shellune_terminal::ExitReport::Clean,
                            Some(code) => shellune_terminal::ExitReport::Code {
                                code: i32::try_from(code).unwrap_or(i32::MAX),
                            },
                        }));
                    }
                }
            }
        }

        if let Some((host_id, prompt)) = prompt {
            if let Some(attempt) = self.attempts.iter_mut().find(|attempt| attempt.id == id) {
                attempt.asking_hop = host_id;
            }
            if let Some(panel) = self.panel_for_mut(id) {
                panel.set_status(RemoteStatus::VerifyingHost(prompt));
            }
            cx.notify();
        }
        if let Some((host_id, label, command)) = proxy {
            if let Some(attempt) = self.attempts.iter_mut().find(|attempt| attempt.id == id) {
                attempt.asking_hop = Some(host_id);
            }
            if let Some(panel) = self.panel_for_mut(id) {
                panel.set_status(RemoteStatus::ConfirmingProxy {
                    host_id,
                    label,
                    command,
                });
            }
            cx.notify();
        }
        for hop in ready {
            if !self.commit_ready_trust(id, hop, cx) {
                return false;
            }
        }
        if let Some(error) = failed_remote {
            self.close_attempt_transport(id);
            self.on_remote_failed(id, &error, cx);
            return false;
        }
        if let Some(failure) = failed_chain {
            self.close_attempt_transport(id);
            self.on_chain_failed(id, &failure, cx);
            return false;
        }
        if !connected {
            return true;
        }
        let host = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .map(|attempt| attempt.target.clone())
            .or_else(|| self.panel_for(id).map(|panel| panel.host().clone()));
        let ephemeral = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .is_some_and(|attempt| attempt.ephemeral);
        let forward_only = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .is_some_and(|attempt| attempt.forward_only);
        let pending_rules = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .map(|attempt| attempt.pending_rules.clone())
            .unwrap_or_default();
        let Some(link) = self.take_transport(id) else {
            return false;
        };
        let Some(host) = host else {
            self.workspace.session_closed(id);
            return false;
        };
        let show = self.form_session == Some(id);
        self.on_remote_connected(
            id,
            link,
            pending,
            ConnectedSession {
                host,
                ephemeral,
                show,
                forward_only,
                pending_rules,
            },
            cx,
        );
        false
    }

    /// Writes a chain identity after the hop that asked for it is ready.
    ///
    /// Queuing the answer is not enough: a `true` from `answer_host_key` only means
    /// the answer was waiting. The trust file changes once this hop reports ready.
    fn commit_ready_trust(&mut self, id: SessionId, hop: HostId, cx: &mut Context<Self>) -> bool {
        let pending = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .and_then(|attempt| {
                attempt
                    .pending_trust
                    .as_ref()
                    .is_some_and(|pending| pending.host_id == hop)
                    .then(|| attempt.pending_trust.take())
                    .flatten()
            });
        let Some(pending) = pending else {
            return true;
        };
        let previous = self.trust_book.clone();
        let now = remote::now_unix();
        if let Err(error) =
            remote::accept_prompted_identity(&mut self.trust_book, &pending.prompt, now)
        {
            self.trust_book = previous;
            self.abort_for_trust(id, error.to_string(), cx);
            return false;
        }
        if let Err(error) = self.trust.save(&self.trust_book) {
            self.trust_book = previous;
            self.abort_for_trust(id, error.to_string(), cx);
            return false;
        }
        true
    }

    /// Closes a chain whose accepted identity could not be stored, before a terminal opens.
    fn abort_for_trust(&mut self, id: SessionId, reason: String, cx: &mut Context<Self>) {
        self.close_attempt_transport(id);
        let strings = self.workspace.strings();
        if let Some(panel) = self.panel_for_mut(id) {
            panel.fail(RemoteFailure::new(
                reason.clone(),
                strings.remote_close,
                true,
            ));
        }
        self.workspace
            .session_failed(id, SessionFailure::ConnectionFailed { reason });
        cx.notify();
    }

    /// Turns a session that reached the remote shell into a terminal of its own.
    fn on_remote_connected(
        &mut self,
        id: SessionId,
        mut link: AttemptLink,
        pending: Vec<SourceEvent>,
        connected: ConnectedSession,
        cx: &mut Context<Self>,
    ) {
        let ConnectedSession {
            host,
            ephemeral,
            show,
            forward_only,
            pending_rules,
        } = connected;
        let Some(tag) = self.workspace.session(id).map(|session| session.tag()) else {
            return;
        };
        let target = host.target();
        // The browsable side of this connection is taken before the terminal owns the
        // session: the file view then asks on the connection that is already
        // authenticated, and never has to ask for credentials a second time.
        let file_link = link.take_file_link();
        let forward_link = link.take_forward_link();
        if forward_only {
            if let Some(forward_link) = forward_link {
                self.forwards.attach(id, forward_link);
            }
            self.forward_carriers.insert(id, link);
            self.workspace.session_verified_and_authenticated(id);
            self.workspace.session_connected(id);
            if ephemeral {
                self.unsaved.push((id, host.clone()));
            } else if self.book.host(host.id()).is_some() {
                self.record_host_connection(host.id(), remote::now_unix(), cx);
            }
            if show {
                self.form_session = None;
                if let Some(panel) = self.remote.as_mut() {
                    panel.clear_secrets();
                }
                self.remote = None;
                self.chain = None;
                self.showing_connection_form = false;
            }
            self.active = Some(id);
            self.pending_forward_rules = pending_rules;
            self.start_pending_forwards(id, cx);
            self.present_forwards(cx);
            return;
        }
        if let Some(forward_link) = forward_link {
            self.forwards.attach(id, forward_link);
            self.watch_forwards(cx);
        }
        // The transfer side is taken here too, for the same reason: a transfer runs on
        // the connection the shell already authenticated and never asks for credentials
        // of its own.
        let transfer_link = link.take_transfer_link();
        let grid = GridSize::initial();
        let font_size = self.workspace.local_font_size();
        let locale = self.workspace.locale();
        let mode = self.workspace.layout().session_mode(id);
        let pane = cx.new(move |pane_cx| {
            let mut pane = match link {
                AttemptLink::Remote(session) => TerminalPane::start_remote(
                    session, tag, target, grid, font_size, locale, pane_cx,
                ),
                AttemptLink::Chain(session) => TerminalPane::start_chain(
                    session, tag, target, grid, font_size, locale, pane_cx,
                ),
            };
            pane.apply_presentation(mode);
            pane
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

    /// Records a chain failure using the chain's own text, which names the hop.
    fn on_chain_failed(
        &mut self,
        id: SessionId,
        failure: &shellune_ssh::ChainFailure,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.panel_for_mut(id) {
            panel.fail(RemoteFailure::from_chain(failure));
        }
        self.workspace.session_failed(id, failure.session_failure());
        cx.notify();
    }

    /// Queues a chain trust answer and writes nothing until that hop is ready.
    fn answer_chain_trust(
        &mut self,
        id: SessionId,
        accept: bool,
        prompt: shellune_ssh::HostKeyPrompt,
        cx: &mut Context<Self>,
    ) {
        let strings = self.workspace.strings();
        let host_id = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .and_then(|attempt| attempt.asking_hop);
        let delivered = self
            .link_mut(id)
            .is_some_and(|link| link.answer_host_key(host_id, accept));
        if !accept {
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
        if !delivered {
            if let Some(panel) = self.panel_for_mut(id) {
                panel.fail(RemoteFailure::new(
                    strings.trust_undelivered_note,
                    strings.remote_close,
                    true,
                ));
            }
            self.workspace.session_failed(
                id,
                SessionFailure::ConnectionFailed {
                    reason: strings.trust_undelivered_note.to_owned(),
                },
            );
            cx.notify();
            return;
        }
        if let Some(host_id) = host_id
            && let Some(attempt) = self.attempts.iter_mut().find(|attempt| attempt.id == id)
        {
            attempt.pending_trust = Some(PendingTrust { host_id, prompt });
        }
        if let Some(panel) = self.remote.as_mut() {
            panel.set_status(RemoteStatus::Connecting);
        }
        cx.notify();
    }

    /// Confirms or refuses the ProxyCommand that is waiting. A false answer starts nothing.
    fn answer_proxy(&mut self, allow: bool, cx: &mut Context<Self>) {
        let Some(host_id) = self
            .remote
            .as_ref()
            .and_then(|panel| panel.status().proxy_question())
            .map(|(host_id, _, _)| host_id)
        else {
            return;
        };
        let Some(id) = self.form_session else {
            return;
        };
        let strings = self.workspace.strings();
        let delivered = self
            .link_mut(id)
            .is_some_and(|link| link.confirm_proxy(host_id, allow));
        if !delivered {
            if let Some(panel) = self.panel_for_mut(id) {
                panel.fail(RemoteFailure::new(
                    strings.trust_undelivered_note,
                    strings.remote_close,
                    true,
                ));
            }
            cx.notify();
            return;
        }
        if let Some(panel) = self.remote.as_mut() {
            panel.set_status(RemoteStatus::Connecting);
        }
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
        if self
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .is_some_and(|attempt| attempt.link.as_ref().is_some_and(AttemptLink::is_chain))
        {
            self.answer_chain_trust(id, accept, prompt, cx);
            return;
        }

        if !accept {
            if let Some(link) = self.link_mut(id) {
                let _ = link.answer_host_key(None, false);
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
            .link_mut(id)
            .is_some_and(|link| link.answer_host_key(None, true));
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
        if let Some(link) = self.link_mut(id) {
            let _ = link.answer_host_key(None, false);
        }
        if let Some(mut link) = self.take_transport(id) {
            link.close();
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
        let target = self
            .remote
            .as_ref()
            .map(|panel| panel.host().clone())
            .or_else(|| {
                self.attempts
                    .iter()
                    .find(|attempt| attempt.id == id)
                    .map(|attempt| attempt.target.clone())
            });
        let Some(target) = target else {
            return;
        };
        self.attempts.retain(|attempt| attempt.id != id);
        self.attempts.push(Attempt {
            id,
            generation,
            link: Some(AttemptLink::Remote(scripted.take_session())),
            watching: false,
            panel: None,
            chain: None,
            pending_trust: None,
            asking_hop: None,
            target,
            ephemeral,
            forward_only: false,
            pending_rules: Vec::new(),
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

    /// Why the quick-connection form refused to dial, when it did.
    #[cfg(test)]
    pub(crate) fn quick_refusal(&self) -> Option<&str> {
        self.quick_error.as_deref()
    }

    /// The address still in the quick-connection form.
    #[cfg(test)]
    pub(crate) fn quick_address_text(&self, cx: &gpui::App) -> String {
        self.quick_address.read(cx).text().to_owned()
    }

    /// The user still in the quick-connection form.
    #[cfg(test)]
    pub(crate) fn quick_user_text(&self, cx: &gpui::App) -> String {
        self.quick_user.read(cx).text().to_owned()
    }

    /// The last save-as-host result: failed, reason, next step.
    #[cfg(test)]
    pub(crate) fn quick_save_report(&self) -> Option<(bool, String, String)> {
        self.quick_save_notice
            .as_ref()
            .map(|notice| (notice.failed, notice.reason.clone(), notice.next.clone()))
    }

    /// How many connection attempts still have a transport.
    #[cfg(test)]
    pub(crate) fn pending_attempt_count(&self) -> usize {
        self.attempts
            .iter()
            .filter(|attempt| attempt.link.is_some())
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
            self.chain = None;
            self.showing_connection_form = false;
            self.workspace.clear_armed();
            cx.notify();
        }
        self.reconnect = None;
        self.pending_ephemeral = None;
        self.chain = None;
        self.forward_only_dial = false;
        self.pending_forward_rules.clear();
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
        let mode = self.workspace.layout().session_mode(id);
        let pane = cx.new(|pane_cx| {
            let mut pane = TerminalPane::start(shell, tag, grid, font_size, locale, pane_cx);
            pane.apply_presentation(mode);
            pane
        });
        let state = pane.read(cx).state().clone();
        if !state.accepts_input() {
            let reason = state
                .failure()
                .map_or_else(|| state.label(locale), |failure| failure.to_string());
            // A process that never ran is not a session. A new tab is removed. An
            // adopt puts the existing leaf back to waiting so its target can be tried
            // again; the reason stays on the workspace that still shows that leaf.
            if !self.workspace.revert_failed_adopt(id) {
                self.workspace.session_forgotten(id);
            }
            self.local_error = Some(reason.clone());
            self.workspace
                .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
            cx.notify();
            return;
        }

        self.workspace.clear_adopted_launch();
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
        self.checkpoint_layout();
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
        let mode = self.workspace.layout().session_mode(id);
        let pane = cx.new(|pane_cx| {
            let mut pane = TerminalPane::start(shell, tag, grid, font_size, locale, pane_cx);
            pane.apply_presentation(mode);
            pane
        });
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
        // A saved command that was waiting for its confirmation is answered in the pane, so
        // every change a pane reports is also a chance to take that note off the corner
        // when the prompt it described is gone (R6).
        self.settle_snippet_note_if_answered(cx);
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
        // The terminal keeps nothing about *what* happened, only that a bell rang or a
        // shell-reported command ended (R7). The window is what knows which tab is on
        // screen, so it decides here whether the news marks a tab or was already seen.
        let notices = pane.update(cx, |pane, _cx| pane.take_attention());
        self.collect_attention(id, notices, cx);
        let state = pane.read(cx).state().clone();
        let presentation = pane.read(cx).presentation();
        if self
            .workspace
            .layout_mut()
            .set_session_mode(id, presentation)
        {
            self.checkpoint_layout();
        }
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
                self.forwards.detach(id);
                self.end_files(id);
            }
            PaneState::Failed(failure) => {
                self.workspace.session_failed(id, failure);
                self.transfers
                    .session_lost(id, shellune_core::TransferFailure::ConnectionLost);
                self.forwards.detach(id);
                self.end_files(id);
            }
        }
        cx.notify();
    }

    /// Records a terminal's background notices on its tab, unless that tab is on
    /// screen (R7).
    ///
    /// A bell in a background shell, or a command end the shell reported there, marks
    /// that tab and nothing else: no tab is switched, no focus moves and no toast is
    /// raised, so the terminal the user is typing into is not interrupted. The mark
    /// says which of the two happened; it carries no output, command line or credential
    /// text, because the terminal only keeps those two flags.
    ///
    /// "On screen" is the tab, not the focused pane: a split tab shows every one of its
    /// panes at once, so a bell in the pane next to the one being typed into is news
    /// the user is already looking at.
    fn collect_attention(
        &mut self,
        id: SessionId,
        notices: TerminalAttention,
        cx: &mut Context<Self>,
    ) {
        if notices.is_empty() {
            return;
        }
        if self.is_viewing_session(id) {
            // The user is looking at that tab, so there is nothing to remind them of; a
            // mark left over from a moment ago would claim news they have seen. The whole
            // tab is cleared, not one kind: a split tab is looked at all at once.
            if self.workspace.clear_session_attention(id) {
                cx.notify();
            }
            return;
        }
        let mut marked = false;
        for (kind, present) in [
            (AttentionKind::Bell, notices.bell),
            (AttentionKind::CommandFinished, notices.finished),
        ] {
            if present && self.workspace.note_attention(id, kind) {
                marked = true;
            }
        }
        if marked {
            cx.notify();
        }
    }

    /// Whether one session's terminal is on screen.
    ///
    /// This asks about the tab, not about the one focused pane: a tab that is split
    /// shows all of its panes at once, so every session the layout says is on screen is
    /// being watched. The focused pane only decides where typed input goes (R7).
    fn is_viewing_session(&self, id: SessionId) -> bool {
        self.terminal_on_screen() && self.workspace.layout().sessions_on_screen().contains(&id)
    }

    /// Whether the terminals of the tab on screen are the ones being drawn.
    ///
    /// A page other than the workspace hides them, and so does a connection form while
    /// it is the surface the user is filling in.
    fn terminal_on_screen(&self) -> bool {
        self.session_terminal
            && !self.showing_connection_form
            && self.shows_terminal(self.workspace.destination())
    }

    /// Clears the background marks of the tab the user is looking at (R7).
    ///
    /// Looking at a tab is what clears its bell and command-end marks; a disconnect is
    /// not one of these marks, so it keeps showing its state and reason until the
    /// session reconnects, closes or the user confirms it. The tab is read from the
    /// layout rather than from the focused session, so a split tab is cleared as a whole
    /// as soon as it is on screen.
    fn clear_viewed_attention(&mut self, cx: &mut Context<Self>) {
        if !self.terminal_on_screen() {
            return;
        }
        let Some(tab) = self.workspace.layout().active_tab_id() else {
            return;
        };
        if self.workspace.clear_attention(tab) {
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------
    // Remote connections: the panel (R4, R6)
    // -----------------------------------------------------------------------

    /// The quick-connection form. It never writes a host by itself.
    fn quick_panel(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        // The dialog around the form carries its title, hint and close button.
        let mut card = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(metrics.scale(spacing::SNUG + 2.0))
            .debug_selector(|| "quick.panel".to_owned())
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
        let method = self.quick_method;
        let mut methods = components::segmented(metrics);
        for choice in MethodChoice::ALL {
            let selected = choice == method;
            let selector = quick_method_selector(choice);
            methods = methods.child(
                components::segment(choice.label(strings), selected, metrics)
                    .id(("quick.method", choice as usize))
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.quick_method = choice;
                        this.quick_error = None;
                        cx.notify();
                    })),
            );
        }
        card = card
            .child(div().flex().child(methods))
            .child(caption_line(method.hint(strings).to_owned(), metrics));
        if method.needs_password() {
            card = card.child(self.remote_field(
                SharedString::from("quick.password"),
                strings.auth_password,
                self.quick_password.clone(),
            ));
        }
        if method.needs_key_path() {
            card = card.child(self.remote_field(
                SharedString::from("quick.key"),
                strings.auth_key,
                self.quick_key.clone(),
            ));
        }
        if method.needs_passphrase() {
            card = card.child(self.remote_field(
                SharedString::from("quick.passphrase"),
                strings.auth_key_passphrase,
                self.quick_passphrase.clone(),
            ));
        }
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
            .filter(|(_, attempt)| attempt.link.is_some() || attempt.panel.is_some())
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

        // The dialog around this panel carries the host, its state and the close
        // button; the panel is the conversation with the server.
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(metrics.scale(spacing::SNUG))
            .debug_selector(|| "remote.panel".to_owned());
        let forward_only = self.forward_only_dial
            || self.form_session.is_some_and(|id| {
                self.attempts
                    .iter()
                    .any(|attempt| attempt.id == id && attempt.forward_only)
            });
        if forward_only {
            column = column.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "remote.forward_only".to_owned())
                    .child(strings.forward_connect_only),
            );
        }

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

        if let Some((_host_id, label, command)) = status.proxy_question() {
            let label = label.to_owned();
            let command = command.to_owned();
            column = column.child(
                components::banner(Tone::Warning, metrics)
                    .gap(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "remote.proxy".to_owned())
                    .child(
                        div()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(strings.remote_proxy_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "remote.proxy.hop".to_owned())
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "remote.proxy.command".to_owned())
                            .child(SharedString::from(command)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .child(self.text_button(
                                SharedString::from("remote.proxy.confirm"),
                                strings.remote_proxy_confirm,
                                Variant::Primary,
                                cx,
                                move |view, cx| view.answer_proxy(true, cx),
                            ))
                            .child(self.text_button(
                                SharedString::from("remote.proxy.refuse"),
                                strings.remote_proxy_refuse,
                                Variant::Danger,
                                cx,
                                move |view, cx| view.answer_proxy(false, cx),
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
        let hop_label = panel.hop_label().map(str::to_owned);

        let mut form = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG - 2.0))
            .debug_selector(|| "remote.form".to_owned());

        if let Some(hop_label) = hop_label {
            form = form.child(
                div()
                    .text_size(metrics.label_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "remote.hop".to_owned())
                    .child(SharedString::from(hop_label)),
            );
        }

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

        // The dialog around the panel carries its title and close button.
        let mut card = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(metrics.scale(spacing::SNUG + 2.0))
            .debug_selector(|| "hosts.import.panel".to_owned())
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
        if !host.jumps().is_empty() {
            let hops: Vec<String> = host.jumps().iter().map(|jump| jump.label()).collect();
            row = row.child(caption_line(
                format!("{}: {}", strings.import_jump_label, hops.join(", ")),
                metrics,
            ));
        }
        if let Some(command) = host.proxy_command() {
            row = row.child(caption_line(
                format!("{}: {command}", strings.import_proxy_label),
                metrics,
            ));
        }
        for forward in host.forwards() {
            row = row.child(caption_line(
                format!("{}: {}", strings.import_forward_label, forward.summary()),
                metrics,
            ));
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
                .child(self.row_icon_button(
                    SharedString::from(format!("hosts.forwards.{index}")),
                    Icon::Forward,
                    strings.forward_open,
                    false,
                    cx,
                    move |view, cx| {
                        view.show_forwards_scope(
                            crate::app::forwarding::ForwardScope::Host(id),
                            cx,
                        );
                    },
                ))
                .child(self.row_icon_button(
                    SharedString::from(format!("hosts.edit.{index}")),
                    Icon::Pencil,
                    strings.host_edit,
                    false,
                    cx,
                    move |view, cx| view.open_edit_host(&edited, cx),
                ))
                .child(self.row_icon_button(
                    SharedString::from(format!("hosts.delete.{index}")),
                    Icon::Trash,
                    strings.host_delete,
                    true,
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
                    .child(self.row_icon_button(
                        SharedString::from(format!("hosts.recent.forwards.{index}")),
                        Icon::Forward,
                        strings.forward_open,
                        false,
                        cx,
                        move |view, cx| {
                            view.show_forwards_scope(
                                crate::app::forwarding::ForwardScope::Host(id),
                                cx,
                            );
                        },
                    ))
                    .child(self.row_icon_button(
                        SharedString::from(format!("hosts.recent.edit.{index}")),
                        Icon::Pencil,
                        strings.host_edit,
                        false,
                        cx,
                        move |view, cx| view.open_edit_host(&edited, cx),
                    ))
                    .child(self.row_icon_button(
                        SharedString::from(format!("hosts.recent.delete.{index}")),
                        Icon::Trash,
                        strings.host_delete,
                        true,
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

    /// A secondary row action as a square icon with its name in a tooltip, so the
    /// row's one labelled button is the action people come for. A destructive
    /// action turns the danger colour under the pointer.
    fn row_icon_button(
        &self,
        id: SharedString,
        icon: Icon,
        label: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let selector = id.to_string();
        let button = if danger {
            components::danger_icon_button(icon, metrics)
        } else {
            components::icon_only_button(icon, metrics)
        };
        button
            .id(id)
            .debug_selector(move || selector.clone())
            .tooltip(components::tooltip(label))
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
            Destination::Hosts
            | Destination::Transfers
            | Destination::Forwards
            | Destination::Settings => self.workspace.item_count(destination),
        }
    }

    /// Steps the theme: system, light, dark.
    pub fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme_choice = self.theme_choice.next();
        self.repaint_terminals(cx);
        cx.notify();
    }

    /// Switches short motion off or on. Does not read a system reduced-motion flag.
    pub fn cycle_motion(&mut self, cx: &mut Context<Self>) {
        self.workspace.cycle_motion();
        cx.notify();
    }

    /// The name of the current motion setting.
    fn motion_label(&self) -> &'static str {
        let strings = self.strings();
        if self.workspace.motion().allows_animation() {
            strings.motion_full
        } else {
            strings.motion_reduced
        }
    }

    /// True when the labelled rail plus the page's main action would not fit.
    ///
    /// Window caption buttons stay a fixed 46px each; they are not part of the text scale.
    fn viewport_forces_icon_rail(&self) -> bool {
        let metrics = self.metrics();
        let caption = if Platform::current() == Platform::Windows {
            46.0 * 3.0
        } else {
            0.0
        };
        self.viewport_width < metrics.rail_width() + metrics.scale(280.0) + px(caption)
    }

    /// The theme the user chose.
    #[allow(dead_code)]
    #[must_use]
    pub fn theme_choice(&self) -> theme::ThemeChoice {
        self.theme_choice
    }

    /// The terminal's palette choice.
    pub fn terminal_scheme(&self) -> theme::TerminalScheme {
        self.terminal_scheme
    }

    /// Changes the terminal's palette and redraws every open terminal in it.
    pub(super) fn set_terminal_scheme(
        &mut self,
        scheme: theme::TerminalScheme,
        cx: &mut Context<Self>,
    ) {
        self.terminal_scheme = scheme;
        self.repaint_terminals(cx);
        cx.notify();
    }

    /// Whether `handle` is the window or a page, rather than a field or a terminal.
    fn is_page_focus(&self, handle: &FocusHandle) -> bool {
        handle == &self.root_focus || self.destination_focus.iter().any(|item| item == handle)
    }

    /// Remembers a field or terminal while one has the keyboard.
    ///
    /// Page focus does not replace it: a click that opens an overlay often blurs the
    /// field first, and the page focus that fills the gap is not where the user was.
    fn note_content_focus(&mut self, window: &Window, cx: &gpui::App) {
        if self.palette.is_some()
            || self.host_fields.is_some()
            || self.tab_menu.is_some()
            || self.session_menu.is_some()
        {
            return;
        }
        if let Some(handle) = window.focused(cx)
            && !self.is_page_focus(&handle)
        {
            self.focus_return = Some(handle.downgrade());
        }
    }

    /// Whether the custom shell path field has the keyboard.
    #[cfg(test)]
    pub fn shell_path_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.shell_path.read(cx).focus_handle().is_focused(window)
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
    ///
    /// A window that is already forcing the icon rail does not store that as the
    /// user's choice, so widening it brings the labels back.
    pub fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        if self.viewport_forces_icon_rail() {
            return;
        }
        self.rail_collapsed = !self.rail_collapsed;
        cx.notify();
    }

    /// Whether the sidebar is collapsed.
    #[allow(dead_code)]
    #[must_use]
    pub fn rail_collapsed(&self) -> bool {
        self.rail_collapsed
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

    /// The menu under the header's "▾": the local shells first, then the ways to
    /// reach a server.
    ///
    /// Picking a shell opens it in a new tab at once, which is what the control is
    /// for; the choice is also remembered as the one "+" opens next, and is marked
    /// with a check. A transparent layer behind the menu closes it on any press
    /// outside. The menu hangs from the control, whose bounds the top bar records as
    /// it draws.
    fn shell_menu_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.shell_menu_open {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let viewport = window.viewport_size();
        let anchor = self.shell_menu_anchor.get();

        let section = |label: &'static str| {
            div()
                .px(metrics.scale(spacing::TIGHT))
                .pt(metrics.scale(spacing::HAIRLINE + 2.0))
                .pb(metrics.scale(2.0))
                .text_size(metrics.caption_size())
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(label)
        };
        let divider = || {
            div()
                .h(px(1.0))
                .mx(metrics.scale(spacing::HAIRLINE))
                .my(metrics.scale(spacing::HAIRLINE))
                .bg(theme::swatch(palette::BORDER))
        };

        let mut menu = div()
            .flex()
            .flex_col()
            .w(metrics.scale(320.0))
            .p(metrics.scale(spacing::HAIRLINE + 1.0))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
            .debug_selector(|| "shell.picker".to_owned())
            .child(section(ux.home_local_section));

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
                    Icon::Terminal,
                    true,
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
                self.menu_item(
                    Icon::FolderOpen,
                    false,
                    format!("{}{ELLIPSIS}", strings.shell_custom_button),
                    None,
                    false,
                )
                .id("shell.custom")
                .debug_selector(|| "shell.custom".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.shell_menu_open = false;
                    this.open_custom_shell(window, cx);
                })),
            )
            .child(divider())
            .child(section(ux.home_hosts_section))
            .child(
                self.menu_item(
                    Icon::Plug,
                    false,
                    strings.quick_connect.to_owned(),
                    None,
                    false,
                )
                .id("menu.quick")
                .debug_selector(|| "menu.quick".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.open_quick_connect(window, cx);
                })),
            )
            .child(
                self.menu_item(
                    Icon::Plus,
                    false,
                    strings.primary_hosts.to_owned(),
                    None,
                    false,
                )
                .id("menu.new_host")
                .debug_selector(|| "menu.new_host".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.shell_menu_open = false;
                    this.select(Destination::Hosts, window, cx);
                    this.open_create_host(cx);
                })),
            )
            .child(
                self.menu_item(
                    Icon::Import,
                    false,
                    strings.import_open.to_owned(),
                    None,
                    false,
                )
                .id("menu.import")
                .debug_selector(|| "menu.import".to_owned())
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.shell_menu_open = false;
                    this.select(Destination::Hosts, window, cx);
                    this.open_import(cx);
                })),
            );

        Some(
            overlay_layer().child(
                div()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("shell.menu.backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|this, _event, _window, cx| {
                                this.close_shell_menu(cx);
                            })),
                    )
                    .child(
                        gpui::anchored()
                            .position(gpui::point(
                                anchor.left(),
                                anchor.bottom() + metrics.scale(spacing::HAIRLINE + 2.0),
                            ))
                            .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                            .child(div().occlude().child(menu)),
                    ),
            ),
        )
    }

    /// One line of a menu: an icon, a name, an optional detail on the right and,
    /// for the current choice, a check.
    fn menu_item(
        &self,
        icon: Icon,
        accent: bool,
        label: String,
        detail: Option<String>,
        selected: bool,
    ) -> Div {
        let metrics = self.metrics();
        let hover = theme::swatch(palette::SURFACE_HOVER);
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
            .hover(move |style| style.bg(hover))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(24.0))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(if accent {
                        palette::ACCENT_TINT
                    } else {
                        palette::SURFACE_SELECTED
                    }))
                    .child(icons::icon(
                        icon,
                        metrics.small_icon_size(),
                        theme::swatch(if accent {
                            palette::ACCENT
                        } else {
                            palette::TEXT_SECONDARY
                        }),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(SharedString::from(label)),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_none()
                    .max_w(metrics.scale(130.0))
                    .truncate()
                    .font_family(terminal::monospace_family())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(detail))
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(16.0))
                    .when(selected, |slot| {
                        slot.child(icons::icon(
                            Icon::Check,
                            metrics.small_icon_size(),
                            theme::swatch(palette::ACCENT),
                        ))
                    }),
            )
    }

    /// Closes the shell menu, and with it the path field inside it.
    fn close_shell_menu(&mut self, cx: &mut Context<Self>) {
        if self.shell_menu_open {
            self.shell_menu_open = false;
            cx.notify();
        }
    }

    /// Opens or closes the typed-path field, giving it the keyboard when it opens.
    /// Opens the dialog that starts any program by its path, with the keyboard in
    /// the path field.
    fn open_custom_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.custom_shell_open = true;
        let handle = self.shell_path.read(cx).focus_handle().clone();
        window.focus(&handle);
        cx.notify();
    }

    /// Closes the custom shell dialog without starting anything.
    pub(super) fn close_custom_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.custom_shell_open {
            self.custom_shell_open = false;
            self.restore_focus(window, cx);
            cx.notify();
        }
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
                    Destination::Forwards => strings.empty_forwards_title,
                    Destination::Settings => copy::ux().settings_title,
                },
                match destination {
                    Destination::LocalShells => strings.empty_local_body,
                    Destination::Hosts => strings.empty_hosts_body,
                    Destination::Sessions => strings.empty_sessions_body,
                    Destination::Transfers => strings.empty_transfers_body,
                    Destination::Forwards => strings.empty_forwards_body,
                    Destination::Settings => copy::ux().settings_summary,
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
            Destination::Transfers => self.transfer_rows(list, cx),
            Destination::Forwards => self.forward_rows(list, cx),
            Destination::Settings => list,
        }
    }

    /// Whether a destination shows the selected terminal.
    ///
    /// Only the terminals destination does, and only while a terminal or a waiting
    /// pane exists. A connection form is a dialog over the page, so it never hides
    /// the terminal behind it.
    fn shows_terminal(&self, destination: Destination) -> bool {
        if destination != Destination::LocalShells {
            return false;
        }
        let live = self.active.is_some_and(|id| self.pane_for(id).is_some());
        // A waiting pane is drawn in the workspace. A session that is only
        // connecting still has no terminal, so it does not take the page over.
        live || self.workspace.layout().has_pending()
    }

    /// The content under the top bar: the settings, a terminal, or the hosts home.
    fn destination_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let destination = self.workspace.destination();
        if destination == Destination::Settings {
            return self.settings_page(cx);
        }
        if self.shows_terminal(destination) {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .id("content.scroll")
                .debug_selector(|| "content".to_owned())
                .track_focus(self.destination_focus(destination))
                .child(self.terminal_panel(cx));
        }
        self.home_page(cx)
    }

    /// The selected terminal, the session it belongs to and the shell it runs.
    ///
    /// The terminal fills the panel edge to edge with the bar of its session on top,
    /// so the name, the state and the actions are read in the same place as the output.
    fn terminal_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        if self.workspace.layout().active_tab().is_some() {
            return self.layout_terminal(cx);
        }
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
            match id.filter(|id| self.files.is_open(*id)) {
                Some(open) => frame = frame.child(self.files_view(open, cx)),
                None => frame = frame.child(pane.clone()),
            }
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
        column.child(frame)
    }

    /// The terminal, with the file view beside it while one is open (R9).
    /// The terminal column, or - while the file view of this session is open - the
    /// file manager in its place: this computer and the server side by side, with
    /// the transfers underneath. The session bar stays on top either way.
    fn files_view(&self, id: SessionId, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let wide = self.files_side_by_side();
        let shown = self.files.shown(id).unwrap_or(FileSource::Remote);

        let mut panes = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .overflow_hidden()
            // A removed selector keeps its last bounds, so the mode has to be a
            // selector that exists only in the frame that drew it.
            .debug_selector(move || {
                if wide {
                    "files.layout.dual".to_owned()
                } else {
                    "files.layout.single".to_owned()
                }
            });
        if wide || shown == FileSource::Local {
            panes = panes.child(self.files_panel(id, FileSource::Local, cx));
        }
        if wide {
            panes = panes.child(
                div()
                    .flex_none()
                    .w(px(1.0))
                    .h_full()
                    .bg(theme::swatch(palette::BORDER)),
            );
        }
        if wide || shown == FileSource::Remote {
            panes = panes.child(self.files_panel(id, FileSource::Remote, cx));
        }

        let switch = (!wide).then(|| {
            let segment = |selector: &'static str, label: &'static str, source: FileSource| {
                components::segment(label, shown == source, metrics)
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.files_show(id, source, cx);
                    }))
            };
            components::segmented(metrics)
                .child(segment(
                    "files.show.local",
                    ux.files_this_computer,
                    FileSource::Local,
                ))
                .child(segment(
                    "files.show.remote",
                    strings.files_show_remote,
                    FileSource::Remote,
                ))
        });
        // The switch is only needed when the window is too narrow for both sides.
        let toolbar = switch.map(|switch| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(metrics.scale(40.0))
                .px(metrics.scale(spacing::SNUG))
                .bg(theme::swatch(palette::SURFACE))
                .border_b_1()
                .border_color(theme::swatch(palette::BORDER))
                .child(switch)
        });

        // The transfer strip takes room only while there is something to show.
        let transfers = self.transfers_strip_shown(id).then(|| {
            div()
                .id("files.transfers")
                .flex()
                .flex_col()
                .flex_none()
                .max_h(metrics.scale(180.0))
                .overflow_y_scroll()
                .px(metrics.scale(spacing::BASE))
                .py(metrics.scale(spacing::TIGHT))
                .bg(theme::swatch(palette::SURFACE))
                .border_t_1()
                .border_color(theme::swatch(palette::BORDER))
                .child(self.transfers_panel(id, cx))
        });

        div()
            .id("files.panes")
            .debug_selector(|| "files.panes".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .bg(theme::swatch(palette::CANVAS))
            .children(toolbar)
            .child(panes)
            .children(transfers)
    }

    /// "Upload" or "Download" in the interface language.
    fn direction_label(&self, direction: TransferDirection) -> &'static str {
        let strings = self.strings();
        match direction {
            TransferDirection::Upload => strings.transfers_upload,
            TransferDirection::Download => strings.transfers_download,
        }
    }

    /// Uploads files dropped from the system onto the server side, into the
    /// folder it shows.
    fn files_upload_paths(
        &mut self,
        id: SessionId,
        paths: &[std::path::PathBuf],
        cx: &mut Context<Self>,
    ) {
        let directory = self
            .files
            .browser(id)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned);
        let files: Vec<String> = paths
            .iter()
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        if files.is_empty() {
            self.toast(Tone::Warning, copy::ux().drop_folders_unsupported, None, cx);
            return;
        }
        let mut started = 0;
        for local in &files {
            match self.transfers.upload_over(
                id,
                local,
                directory.as_deref(),
                self.session_protocol(id),
                false,
            ) {
                Ok(_) => started += 1,
                Err(problem) => {
                    self.transfer_problem = Some((id, problem));
                }
            }
        }
        if started > 0 {
            self.watch_files(cx);
            let title = copy::fill(
                copy::ux().drop_uploading_files,
                &[("count", &started.to_string())],
            );
            self.toast(Tone::Accent, title, directory, cx);
        }
        cx.notify();
    }

    /// Whether the file view shows its transfer strip: while the session has a
    /// transfer, or a transfer that could not start.
    pub(crate) fn transfers_strip_shown(&self, id: SessionId) -> bool {
        self.transfers.for_session(id).next().is_some()
            || self
                .transfer_problem
                .as_ref()
                .is_some_and(|(session, _)| *session == id)
    }

    /// Whether this computer and the server fit side by side.
    ///
    /// The file view takes the terminal's place, so the whole window width is
    /// available; at 200% text a small window shows one side at a time.
    fn files_side_by_side(&self) -> bool {
        if let Some(wide) = self.files_layout_override {
            return wide;
        }
        self.viewport_width >= self.metrics().scale(FILES_PANEL_WIDTH * 2.0 + 120.0)
    }

    fn files_show(&mut self, id: SessionId, source: FileSource, cx: &mut Context<Self>) {
        self.files.show(id, source);
        cx.notify();
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

    fn files_up(&mut self, id: SessionId, source: FileSource, cx: &mut Context<Self>) {
        // Leaving the directory ends any question about an entry in it: the question
        // names a target in that source, and a later answer would apply wherever the
        // user went next. The other source keeps its own question.
        self.close_files_question_for(id, source);
        self.files.select(id, source, None);
        if source == FileSource::Remote {
            self.files.up(id);
        } else {
            self.files.up_in(id, source);
        }
        cx.notify();
    }

    fn files_refresh(&mut self, id: SessionId, source: FileSource, cx: &mut Context<Self>) {
        if source == FileSource::Remote {
            self.files.refresh(id);
        } else {
            self.files.refresh_in(id, source);
        }
        cx.notify();
    }

    fn files_enter(
        &mut self,
        id: SessionId,
        source: FileSource,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        self.close_files_question_for(id, source);
        self.files.select(id, source, Some(name.to_owned()));
        if source == FileSource::Remote {
            self.files.enter(id, name);
        } else {
            self.files.enter_in(id, source, name);
        }
        cx.notify();
    }

    fn files_go(&mut self, id: SessionId, source: FileSource, path: &str, cx: &mut Context<Self>) {
        self.close_files_question_for(id, source);
        self.files.list_at(id, source, path);
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
            self.clear_files_fields();
        }
    }

    /// Drops the question of one source only.
    fn close_files_question_for(&mut self, id: SessionId, source: FileSource) {
        if self
            .files_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.session() == id && dialog.source() == source)
        {
            self.files_dialog = None;
            self.clear_files_fields();
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
    fn files_new(&mut self, id: SessionId, source: FileSource, cx: &mut Context<Self>) {
        if self.files.browser_in(id, source).is_none() {
            return;
        }
        let placeholder = self.strings().files_new_placeholder;
        let field = cx.new(|cx| TextField::new(placeholder, cx));
        self.open_files_dialog(
            FilesDialog::NewDirectory {
                session: id,
                source,
                text: String::new(),
                problem: None,
            },
            field,
            cx,
        );
    }

    /// Asks what one entry should be called, with its current name already in the field.
    fn files_ask_rename(
        &mut self,
        id: SessionId,
        source: FileSource,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        if self.files.browser_in(id, source).is_none() {
            return;
        }
        let placeholder = self.strings().files_rename_placeholder;
        let field = cx.new(|cx| TextField::with_text(placeholder, name, cx));
        self.open_files_dialog(
            FilesDialog::Rename {
                session: id,
                source,
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
        source: FileSource,
        name: &str,
        kind: FileKind,
        cx: &mut Context<Self>,
    ) {
        if self.files.browser_in(id, source).is_none() {
            return;
        }
        self.files_dialog = Some(FilesDialog::Delete {
            session: id,
            source,
            name: name.to_owned(),
            kind,
            problem: None,
        });
        self.clear_files_fields();
        cx.notify();
    }

    /// Forgets both fields of a question that is being closed.
    ///
    /// A field left over from a closed question would stay bound to a dialog that no
    /// longer exists, and editing it would rewrite the next question's path.
    fn clear_files_fields(&mut self) {
        self.files_field = None;
        self.files_remote_field = None;
    }

    /// Shows a question and binds its field, sending nothing.
    fn open_files_dialog(
        &mut self,
        dialog: FilesDialog,
        field: Entity<TextField>,
        cx: &mut Context<Self>,
    ) {
        self.open_files_dialog_with(dialog, field, None, cx);
    }

    /// Shows a question with an optional second field for its remote path.
    fn open_files_dialog_with(
        &mut self,
        dialog: FilesDialog,
        field: Entity<TextField>,
        remote: Option<Entity<TextField>>,
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
        if let Some(remote) = &remote {
            // The remote path is bound the same way as the local one: what the user
            // types is what the task would use, and typing again clears the last reason
            // an attempt could not start.
            let remote = remote.clone();
            cx.observe(&remote, move |view, text_field, cx| {
                let text = text_field.read(cx).text().to_owned();
                if let Some(dialog) = view.files_dialog.as_mut() {
                    dialog.set_remote(text);
                }
                cx.notify();
            })
            .detach();
        }
        let handle = field.read(cx).focus_handle().clone();
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_view, window, _cx| window.focus(&handle));
        }
        self.files_field = Some(field);
        self.files_remote_field = remote;
        cx.notify();
    }

    /// Uploads one local file into the remote directory on screen.
    ///
    /// The button and a drop onto the remote column both call this, so the transfer
    /// request is the one the upload field already starts.
    fn transfer_to_remote(
        &mut self,
        id: SessionId,
        directory: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(folder) = local_protocol_to_os(directory) else {
            self.transfer_problem = Some((id, TransferProblem::DirectoryUnknown));
            cx.notify();
            return;
        };
        let path = folder.join(name);
        self.start_upload(id, &path.to_string_lossy(), cx);
    }

    /// Downloads one remote file into the local directory on screen.
    ///
    /// The button and a drop onto the local column both call this.
    fn transfer_to_local(
        &mut self,
        id: SessionId,
        directory: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let remote = join_path(directory, name);
        let Some(folder) = self.files.local_os_directory(id) else {
            self.transfer_problem = Some((id, TransferProblem::DirectoryUnknown));
            cx.notify();
            return;
        };
        self.start_download(id, &remote, &folder.to_string_lossy(), cx);
    }

    /// Closes the question without sending anything at all (R11).
    ///
    /// Cancelling is not a request the server has to refuse: the target never left
    /// this process, so there is nothing on the server to undo.
    fn files_cancel_change(&mut self, cx: &mut Context<Self>) {
        self.files_dialog = None;
        self.clear_files_fields();
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
            FilesDialog::NewDirectory {
                session,
                source,
                text,
                ..
            } => {
                if *source == FileSource::Remote {
                    self.files.create_directory(*session, text)
                } else {
                    self.files.create_directory_in(*session, *source, text)
                }
            }
            FilesDialog::Rename {
                session,
                source,
                from,
                text,
                ..
            } => {
                if *source == FileSource::Remote {
                    self.files.rename(*session, from, text)
                } else {
                    self.files.rename_in(*session, *source, from, text)
                }
            }
            FilesDialog::Delete {
                session,
                source,
                name,
                ..
            } => {
                if *source == FileSource::Remote {
                    self.files.delete(*session, name)
                } else {
                    self.files.delete_in(*session, *source, name)
                }
            }
            FilesDialog::Upload { .. } | FilesDialog::Download { .. } => Ok(()),
        };
        match sent {
            Ok(()) => {
                // The question is answered: the report and the next listing say the rest.
                self.files_dialog = None;
                self.clear_files_fields();
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
    /// Announces each change that newly succeeded. A change that failed in part
    /// stays beside its list instead, item by item, until the next change.
    fn announce_file_changes(
        &mut self,
        before: &[(
            (SessionId, FileSource),
            Option<shellune_core::MutationOutcome>,
        )],
        cx: &mut Context<Self>,
    ) {
        let mut done = Vec::new();
        for ((id, source), previous) in before {
            let Some(outcome) = self.files.outcome_in(*id, *source) else {
                continue;
            };
            if previous.as_ref() == Some(outcome) || !outcome.succeeded() {
                continue;
            }
            let strings = self.strings();
            let title = match source {
                FileSource::Local => strings.files_change_done_local,
                FileSource::Remote => strings.files_change_done,
            };
            let detail = match outcome.steps() {
                [one] => one.path().to_owned(),
                steps => format!(
                    "{} {}/{}",
                    strings.files_change_changed,
                    outcome.changed(),
                    steps.len()
                ),
            };
            done.push((title, detail));
        }
        for (title, detail) in done {
            self.toast(Tone::Positive, title, Some(detail), cx);
        }
    }

    fn pump_files(&mut self, cx: &mut Context<Self>) -> bool {
        let done_before: Vec<shellune_core::TransferId> = self
            .transfers
            .transfers()
            .iter()
            .filter(|task| task.state().is_success())
            .map(|task| task.id())
            .collect();
        // The change results before this tick, so a new success can be announced.
        let sessions: Vec<SessionId> = self.panes.iter().map(|open| open.tag.session).collect();
        let outcomes_before: Vec<_> = sessions
            .iter()
            .flat_map(|id| {
                [FileSource::Remote, FileSource::Local]
                    .map(|source| ((*id, source), self.files.outcome_in(*id, source).cloned()))
            })
            .collect();
        let listed = self.files.pump();
        if listed {
            self.announce_file_changes(&outcomes_before, cx);
        }
        let transferred = self.transfers.pump();
        if transferred {
            let mut finished = Vec::new();
            for task in self.transfers.transfers() {
                if !task.state().is_success() || done_before.contains(&task.id()) {
                    continue;
                }
                finished.push((task.direction(), task.destination().to_owned()));
            }
            for (direction, destination) in finished {
                let ux = copy::ux();
                let title = match direction {
                    TransferDirection::Upload => ux.toast_uploaded,
                    TransferDirection::Download => ux.toast_downloaded,
                };
                self.toast(Tone::Positive, title, Some(destination), cx);
            }
            for task in self.transfers.transfers() {
                if !task.state().is_success() || done_before.contains(&task.id()) {
                    continue;
                }
                if !self.files.is_open(task.session()) {
                    continue;
                }
                // The worker reported the bytes as written. The directory is read
                // again so the list cannot show a file the refresh does not return.
                match task.direction() {
                    TransferDirection::Upload => {
                        self.files.refresh(task.session());
                    }
                    TransferDirection::Download => {
                        self.files.refresh_in(task.session(), FileSource::Local);
                    }
                }
            }
        }
        if transferred {
            // The transfers page reads one list, so it is refreshed in the same tick in
            // which the task that changed it was applied.
            self.sync_transfer_section();
        }
        if listed || transferred {
            cx.notify();
        }
        // Leaving the file view does not stop a transfer. The loop stays up while any
        // task can still change, and while a stopped task can still receive the prefix
        // its worker flushed after the last checkpoint.
        let open = self.files.any_open()
            || self.transfers.any_active()
            || self.transfers.awaiting_settlement();
        if !open {
            self.files_polling = false;
        }
        open
    }

    /// Moves a file between the two columns of one session.
    ///
    /// The button on a row and a drop onto the other column both arrive here, so a
    /// drag cannot start a transfer the button would not.
    fn files_accept_drop(
        &mut self,
        id: SessionId,
        onto: FileSource,
        drag: &FileDrag,
        cx: &mut Context<Self>,
    ) {
        if drag.session != id || drag.source == onto {
            return;
        }
        match onto {
            FileSource::Remote => self.transfer_to_remote(id, &drag.directory, &drag.name, cx),
            FileSource::Local => self.transfer_to_local(id, &drag.directory, &drag.name, cx),
        }
    }

    /// Uploads or downloads the named entry of the directory on screen.
    ///
    /// The directory is read from that source's current listing, which is the same
    /// directory a drag from the row would carry.
    fn files_transfer_named(
        &mut self,
        id: SessionId,
        source: FileSource,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(directory) = self
            .files
            .browser_in(id, source)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned)
        else {
            self.transfer_problem = Some((id, TransferProblem::DirectoryUnknown));
            cx.notify();
            return;
        };
        self.files_accept_drop(
            id,
            match source {
                FileSource::Local => FileSource::Remote,
                FileSource::Remote => FileSource::Local,
            },
            &FileDrag {
                session: id,
                source,
                name: name.to_owned(),
                directory,
            },
            cx,
        );
    }

    /// A key pressed while one file column has the keyboard.
    ///
    /// Modified keys are left alone: Escape, Ctrl+L and Ctrl+C belong to the
    /// terminal when it is focused, and a file column must not take them either.
    /// Returns whether the key was a file action.
    fn files_key(
        &mut self,
        id: SessionId,
        source: FileSource,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.modifiers.modified() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "f5" => {
                self.files_refresh(id, source, cx);
                true
            }
            "f2" => {
                if let Some(name) = self.files.selected(id, source).map(str::to_owned) {
                    self.files_ask_rename(id, source, &name, cx);
                }
                true
            }
            "delete" => {
                let name = self.files.selected(id, source).map(str::to_owned);
                let kind = name.as_deref().and_then(|name| {
                    self.files
                        .browser_in(id, source)
                        .and_then(|browser| browser.entry(name))
                        .map(|entry| entry.kind())
                });
                if let (Some(name), Some(kind)) = (name, kind) {
                    self.files_ask_delete(id, source, &name, kind, cx);
                }
                true
            }
            "enter" | "right" => {
                let name = self.files.selected(id, source).map(str::to_owned);
                let open = name.as_deref().is_some_and(|name| {
                    self.files
                        .browser_in(id, source)
                        .and_then(|browser| browser.entry(name))
                        .is_some_and(|entry| entry.is_directory())
                });
                if open && let Some(name) = name {
                    self.files_enter(id, source, &name, cx);
                }
                true
            }
            "left" | "backspace" => {
                self.files_up(id, source, cx);
                true
            }
            "up" => {
                self.files_move_selection(id, source, -1, cx);
                true
            }
            "down" => {
                self.files_move_selection(id, source, 1, cx);
                true
            }
            _ => false,
        }
    }

    /// The names in one listing, in the order the column draws them.
    fn files_entry_names(&self, id: SessionId, source: FileSource) -> Vec<String> {
        self.files
            .browser_in(id, source)
            .and_then(|browser| browser.state().listing())
            .map(|listing| {
                listing
                    .entries()
                    .iter()
                    .map(|entry| entry.name().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Moves the keyboard selection by one row, staying inside the listing.
    fn files_move_selection(
        &mut self,
        id: SessionId,
        source: FileSource,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let names = self.files_entry_names(id, source);
        if names.is_empty() {
            return;
        }
        let current = self
            .files
            .selected(id, source)
            .and_then(|name| names.iter().position(|item| item == name));
        let next = match current {
            Some(index) => {
                let moved = index as isize + delta;
                moved.clamp(0, names.len() as isize - 1) as usize
            }
            None => {
                if delta < 0 {
                    names.len() - 1
                } else {
                    0
                }
            }
        };
        self.files.select(id, source, Some(names[next].clone()));
        cx.notify();
    }

    /// Why one source could not be read, and the action that source can still take.
    ///
    /// Refresh and going up stay on this source. A session reconnect is drawn only
    /// when that session can actually start again. A file channel that failed while
    /// the terminal is still connected is retried with Refresh, and neither failure
    /// closes the terminal.
    fn files_failure(
        &self,
        id: SessionId,
        source: FileSource,
        failure: &BrowseFailure,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let reason = browse_failure_text(failure, source, strings);
        let failure_id = files_element_id(source, "failure");
        let reason_id = files_element_id(source, "failure.reason");
        let note_id = files_element_id(source, "failure.note");
        let refresh_id = files_element_id(source, "failure.refresh");
        let up_id = files_element_id(source, "failure.up");
        let reconnect_id = files_element_id(source, "failure.reconnect");
        let mut actions = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(self.text_button(
                refresh_id,
                strings.files_refresh,
                Variant::Secondary,
                cx,
                move |view, cx| view.files_refresh(id, source, cx),
            ))
            .child(self.text_button(
                up_id,
                strings.files_up,
                Variant::Ghost,
                cx,
                move |view, cx| view.files_up(id, source, cx),
            ));
        let session_can_restart = failure.is_connection_lost()
            && source == FileSource::Remote
            && self.workspace.session(id).is_some_and(may_reconnect);
        if session_can_restart {
            actions = actions.child(self.text_button(
                reconnect_id,
                strings.session_reconnect,
                Variant::Secondary,
                cx,
                move |view, cx| view.reconnect_session(id, cx),
            ));
        }
        let mut card = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::SMALL))
            .p(metrics.scale(spacing::TIGHT))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(move || failure_id.to_string())
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
                    .debug_selector(move || reason_id.to_string())
                    .child(SharedString::from(reason)),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(move || note_id.to_string())
                    .child(SharedString::from(strings.files_terminal_note)),
            )
            .child(actions);
        // A server without an SFTP subsystem cannot be browsed, but its own `scp` can
        // still copy a file. The reason and the next step are both stated, because "the
        // directory cannot be listed" is not the same answer as "nothing can be copied",
        // and only the first one is true. Both directions are offered here as a path the
        // user types: with no listing there is no row to start an upload or a download
        // from, and the card is where the missing entry would otherwise be.
        if source == FileSource::Remote && matches!(failure, BrowseFailure::Unavailable { .. }) {
            let hint_id = files_element_id(source, "failure.scp.hint");
            card = card.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(move || hint_id.to_string())
                    .child(SharedString::from(strings.files_sftp_scp_hint)),
            );
            let upload_id = files_element_id(source, "failure.scp.upload");
            let download_id = files_element_id(source, "failure.scp.download");
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .child(self.text_button(
                        upload_id,
                        strings.transfers_upload,
                        Variant::Secondary,
                        cx,
                        move |view, cx| view.files_ask_upload(id, cx),
                    ))
                    .child(self.text_button(
                        download_id,
                        strings.transfers_download,
                        Variant::Secondary,
                        cx,
                        move |view, cx| view.files_ask_download(id, None, cx),
                    )),
            );
        }
        // The terminal still owns the connection, so Refresh is the action that
        // reads this source again. A session reconnect button would return immediately.
        if failure.is_connection_lost()
            && source == FileSource::Remote
            && !self.workspace.session(id).is_some_and(may_reconnect)
        {
            let hint_id = files_element_id(source, "failure.refresh.hint");
            card = card.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(move || hint_id.to_string())
                    .child(SharedString::from(strings.files_refresh_source)),
            );
        }
        card
    }

    /// One file column: the source it is, the path it shows and the entries of that source.
    /// One side of the file manager: where it is, its toolbar, the path as
    /// breadcrumbs, and the listing.
    fn files_panel(
        &self,
        id: SessionId,
        source: FileSource,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let Some(browser) = self.files.browser_in(id, source) else {
            return div().id(files_element_id(source, "panel"));
        };
        let host = browser.host().to_owned();
        let target = browser.target().to_owned();
        let path = browser.path().map(str::to_owned);
        let changing = browser.is_changing();
        let state = browser.state().clone();
        let remote = source == FileSource::Remote;
        // A listing is where a row's own download lives. Without one there is no entry
        // to click, so both directions are offered as a path the user types: that is the
        // only way a server without an SFTP subsystem copies a file at all.
        let manual_remote = remote && !matches!(&state, BrowseState::Ready(_));
        let (title, subtitle) = match source {
            FileSource::Remote => (host, target),
            FileSource::Local => (ux.files_this_computer.to_owned(), host),
        };
        let shown_path = path
            .as_deref()
            .map(|path| display_path(source, path))
            .unwrap_or_else(|| strings.files_unknown_path.to_owned());

        let source_id = files_element_id(source, "source");
        let host_id = files_element_id(source, "host");
        let header_id = files_element_id(source, "header");
        let path_id = files_element_id(source, "path");

        let mut tools = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(2.0))
            .child(self.row_icon_button(
                files_element_id(source, "up"),
                Icon::ArrowUp,
                strings.files_up,
                false,
                cx,
                move |view, cx| view.files_up(id, source, cx),
            ))
            .child(self.row_icon_button(
                files_element_id(source, "refresh"),
                Icon::Refresh,
                strings.files_refresh,
                false,
                cx,
                move |view, cx| view.files_refresh(id, source, cx),
            ))
            .child(self.row_icon_button(
                files_element_id(source, "new"),
                Icon::FolderPlus,
                strings.files_new,
                false,
                cx,
                move |view, cx| view.files_new(id, source, cx),
            ));
        if remote {
            tools = tools.child(
                self.icon_text_button(
                    SharedString::from("files.upload"),
                    Icon::Upload,
                    strings.transfers_upload,
                    Variant::Soft,
                    cx,
                    move |view, cx| view.files_ask_upload(id, cx),
                )
                .ml(metrics.scale(spacing::HAIRLINE)),
            );
            // With a listing, a download starts from the entry a row names. Without one
            // - a server that offers no SFTP subsystem - the remote path is typed, and
            // then the download has to be reachable from here as well: a copy that only
            // works in one direction is not the file transfer the server can still do.
            if manual_remote {
                tools = tools.child(
                    self.icon_text_button(
                        SharedString::from("files.download"),
                        Icon::Download,
                        strings.transfers_download,
                        Variant::Soft,
                        cx,
                        move |view, cx| view.files_ask_download(id, None, cx),
                    )
                    .ml(metrics.scale(spacing::HAIRLINE)),
                );
            }
        }

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::SNUG - 2.0))
            .debug_selector({
                let header_id = header_id.clone();
                move || header_id.to_string()
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(metrics.scale(32.0))
                    .rounded(px(radius::MEDIUM))
                    .bg(theme::swatch(if remote {
                        palette::ACCENT_TINT
                    } else {
                        palette::SURFACE_SELECTED
                    }))
                    .debug_selector({
                        let source_id = source_id.clone();
                        move || source_id.to_string()
                    })
                    .child(icons::icon(
                        if remote { Icon::Server } else { Icon::Monitor },
                        metrics.small_icon_size(),
                        theme::swatch(if remote {
                            palette::ACCENT
                        } else {
                            palette::TEXT_SECONDARY
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.body_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector({
                                let host_id = host_id.clone();
                                move || host_id.to_string()
                            })
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(subtitle)),
                    ),
            )
            .child(tools);

        // The path, as breadcrumbs that each go back up to that folder.
        let crumbs = path.as_deref().map(path_crumbs).unwrap_or_default();
        let mut crumb_row = div()
            .flex()
            .flex_none()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(2.0))
            .px(metrics.scale(spacing::HAIRLINE + 2.0))
            .py(metrics.scale(3.0))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::CANVAS))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector({
                let path_id = path_id.clone();
                move || path_id.to_string()
            });
        let _ = &shown_path;
        let last = crumbs.len().saturating_sub(1);
        for (index, crumb) in crumbs.iter().enumerate() {
            let crumb_path = crumb.path.clone();
            let label = if index == 0 && source == FileSource::Local {
                strings.files_local_root.to_owned()
            } else {
                crumb.label.clone()
            };
            let crumb_id = files_element_id(source, &format!("crumb.{index}"));
            let hover = theme::swatch(palette::SURFACE_HOVER);
            if index > 0 {
                crumb_row = crumb_row.child(icons::icon(
                    Icon::ChevronRight,
                    metrics.scale(12.0),
                    theme::swatch(palette::TEXT_MUTED),
                ));
            }
            crumb_row = crumb_row.child(
                div()
                    .id(crumb_id.clone())
                    .debug_selector(move || crumb_id.to_string())
                    .px(metrics.scale(spacing::HAIRLINE + 1.0))
                    .py(metrics.scale(1.0))
                    .rounded(px(radius::SMALL))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(if index == last {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_SECONDARY
                    }))
                    .when(index == last, |crumb| crumb.font_weight(FontWeight::MEDIUM))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.files_go(id, source, &crumb_path, cx);
                    }))
                    .child(SharedString::from(label)),
            );
        }

        let body: gpui::AnyElement = match &state {
            BrowseState::Loading => {
                let loading_id = files_element_id(source, "loading");
                div()
                    .flex_none()
                    .p(metrics.scale(spacing::SNUG))
                    .text_size(metrics.label_size())
                    .text_color(Tone::Warning.text())
                    .debug_selector(move || loading_id.to_string())
                    .child(SharedString::from(strings.files_loading))
                    .into_any_element()
            }
            BrowseState::Failed(failure) => self
                .files_failure(id, source, failure, cx)
                .into_any_element(),
            BrowseState::Ready(listing) => {
                let list_id = files_element_id(source, "list");
                let mut list = div()
                    .id(list_id.clone())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .rounded(px(radius::MEDIUM))
                    .bg(theme::swatch(palette::SURFACE))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .debug_selector({
                        let list_id = list_id.clone();
                        move || list_id.to_string()
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        self.files_blank_menu_listener(id, source, cx),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(metrics.scale(28.0))
                            .px(metrics.scale(spacing::SNUG))
                            .border_b_1()
                            .border_color(theme::swatch(palette::BORDER))
                            .text_size(metrics.caption_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(div().flex_1().child(ux.files_name))
                            .child(div().w(metrics.scale(80.0)).child(ux.files_size))
                            .child(div().w(files_actions_width(metrics, source))),
                    );
                if path.as_deref().is_some_and(|path| path != "/") {
                    list = list.child(self.files_parent_row(id, source, cx));
                }
                if listing.is_empty() {
                    let empty_id = files_element_id(source, "empty");
                    list = list.child(
                        div()
                            .flex_none()
                            .p(metrics.scale(spacing::SNUG))
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(move || empty_id.to_string())
                            .child(SharedString::from(strings.files_empty)),
                    );
                }
                for (index, entry) in listing.entries().iter().enumerate() {
                    list = list.child(self.files_row(id, source, index, entry, cx));
                }
                list.into_any_element()
            }
        };

        let panel_id = files_element_id(source, "panel");
        let focus = match source {
            FileSource::Local => &self.files_local_focus,
            FileSource::Remote => &self.files_remote_focus,
        };
        let mut panel = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .p(metrics.scale(spacing::BASE - 2.0))
            .id(panel_id.clone())
            .debug_selector({
                let panel_id = panel_id.clone();
                move || panel_id.to_string()
            })
            .h_full()
            .overflow_hidden()
            .track_focus(focus)
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _window, cx| {
                if view.files_key(id, source, event, cx) {
                    cx.stop_propagation();
                }
            }))
            .can_drop(move |value, _window, _app| {
                value
                    .downcast_ref::<FileDrag>()
                    .is_some_and(|drag| drag.session == id && drag.source != source)
            })
            .drag_over::<FileDrag>(|style, _drag, _window, _cx| {
                style.bg(theme::swatch(palette::ACCENT_TINT))
            })
            .on_drop(cx.listener(move |view, drag: &FileDrag, _window, cx| {
                view.files_accept_drop(id, source, drag, cx);
            }))
            // Files from the system's file manager upload to the folder the server
            // side shows.
            .drag_over::<gpui::ExternalPaths>(move |style, _paths, _window, _cx| {
                if source == FileSource::Remote {
                    style.bg(theme::swatch(palette::ACCENT_TINT))
                } else {
                    style
                }
            })
            .on_drop(
                cx.listener(move |view, paths: &gpui::ExternalPaths, _window, cx| {
                    if source == FileSource::Remote {
                        view.files_upload_paths(id, paths.paths(), cx);
                    }
                }),
            )
            .child(header)
            .child(crumb_row);
        if let Some(question) = self.files_question(id, source, cx) {
            panel = panel.child(question);
        } else if changing {
            let pending_id = files_element_id(source, "pending");
            let pending = match source {
                FileSource::Local => strings.files_change_pending_local,
                FileSource::Remote => strings.files_change_pending,
            };
            panel = panel.child(
                components::banner(Tone::Warning, metrics)
                    .text_size(metrics.caption_size())
                    .debug_selector(move || pending_id.to_string())
                    .child(SharedString::from(pending)),
            );
        }
        panel = panel.child(body);
        if let Some(report) = self.files_report(id, source) {
            panel = panel.child(report);
        }
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
                format!("{state_label} · {}", self.transfer_failure_text(failure))
            }
            _ => state_label.to_owned(),
        };
        if let Some(residue) = task.state().residue() {
            detail.push_str(&format!(" · {}: {residue}", strings.transfers_residue));
        }
        if let Some(hint) = task.resume_hint() {
            detail.push_str(&format!(
                " · {} {}",
                strings.transfers_resume_from, hint.offset
            ));
        }
        detail
    }

    /// Why a transfer failed, in the interface language when the reason is a resume refusal.
    fn transfer_failure_text(&self, failure: &shellune_core::TransferFailure) -> String {
        let strings = self.strings();
        match failure {
            shellune_core::TransferFailure::ResumeRefused { reason } => match reason {
                shellune_core::ResumeRefusal::SourceChanged => strings.transfers_resume_source,
                shellune_core::ResumeRefusal::Unverifiable => strings.transfers_resume_unverifiable,
                shellune_core::ResumeRefusal::Unsupported => strings.transfers_resume_unsupported,
                shellune_core::ResumeRefusal::PartialDamaged => strings.transfers_resume_damaged,
            }
            .to_owned(),
            other => other.to_string(),
        }
    }

    fn transfer_host_label(&self, task: &Transfer) -> String {
        self.workspace
            .session(task.session())
            .map(|session| session.title().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "—".to_owned())
    }

    /// Cancel, resume and start-over for one task. The same actions serve the file
    /// view and the transfer center, so a task can be reached after the files close.
    fn transfer_actions(
        &self,
        mut actions: Div,
        prefix: &'static str,
        index: usize,
        task: &Transfer,
        include_overwrite: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let strings = self.strings();
        let id = task.id();
        // The checkpoint the task may really continue, which is what a resume button
        // promises. A protocol without resume answers nothing here even when a record
        // from elsewhere still names an offset, so SCP never offers such an entry.
        let resumable = task.resume_hint().is_some();
        let refused = matches!(
            task.state().failure(),
            Some(shellune_core::TransferFailure::ResumeRefused { .. })
        );
        if task.is_active() && !task.state().is_awaiting_overwrite() {
            actions = actions.child(self.text_button(
                SharedString::from(format!("{prefix}.cancel.{index}")),
                strings.transfers_cancel,
                Variant::Ghost,
                cx,
                move |view, cx| view.transfer_cancel(id, cx),
            ));
        }
        if include_overwrite && task.state().is_awaiting_overwrite() {
            actions = actions
                .child(self.text_button(
                    SharedString::from(format!("{prefix}.overwrite.replace.{index}")),
                    strings.transfers_overwrite_replace,
                    Variant::Danger,
                    cx,
                    move |view, cx| {
                        view.transfer_decide(id, shellune_core::OverwriteChoice::Replace, cx);
                    },
                ))
                .child(self.text_button(
                    SharedString::from(format!("{prefix}.overwrite.keep.{index}")),
                    strings.transfers_overwrite_keep,
                    Variant::Secondary,
                    cx,
                    move |view, cx| {
                        view.transfer_decide(id, shellune_core::OverwriteChoice::KeepExisting, cx);
                    },
                ));
        }
        if resumable {
            actions = actions.child(self.text_button(
                SharedString::from(format!("{prefix}.resume.{index}")),
                strings.transfers_resume,
                Variant::Secondary,
                cx,
                move |view, cx| view.transfer_retry(id, cx),
            ));
        }
        if matches!(
            task.state(),
            shellune_core::TransferState::Failed { .. }
                | shellune_core::TransferState::Cancelled { .. }
        ) {
            // A checkpoint uses Resume above. Transfer again is the explicit restart
            // from the first byte. A failure with nothing to resume keeps the Retry id
            // the file view already had.
            let (name, label, restart) = if resumable || refused {
                (
                    format!("{prefix}.restart.{index}"),
                    strings.transfers_restart,
                    true,
                )
            } else {
                (
                    format!("{prefix}.retry.{index}"),
                    strings.transfers_retry,
                    false,
                )
            };
            actions = actions.child(self.text_button(
                SharedString::from(name),
                label,
                Variant::Secondary,
                cx,
                move |view, cx| {
                    if restart {
                        view.transfer_restart(id, cx);
                    } else {
                        view.transfer_retry(id, cx);
                    }
                },
            ));
        }
        actions
    }

    /// One transfer of a session: what it moves, how far it is and what it became.
    fn session_transfer_row(
        &self,
        index: usize,
        task: &Transfer,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let tone = Tone::for_transfer_state(task.state());
        let detail = self.transfer_detail(task);
        let mut actions = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::HAIRLINE));
        actions = self.transfer_actions(actions, "transfers", index, task, false, cx);
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
                                "{} · {} · {} → {}",
                                self.direction_label(task.direction()),
                                self.transfer_host_label(task),
                                task.source(),
                                task.destination()
                            ))),
                    )
                    .child(
                        // Which protocol moved these bytes: a task copied with SCP must
                        // not be read as one that used the SFTP subsystem, and the other
                        // way round.
                        components::chip(
                            SharedString::from(format!(
                                "{} {}",
                                self.strings().transfers_protocol_label,
                                task.protocol().label()
                            )),
                            Tone::Neutral,
                            metrics,
                        )
                        .debug_selector(move || format!("transfers.protocol.{index}")),
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

    /// The switch that chooses this one transfer's protocol (R10).
    ///
    /// It starts at the host's own choice and belongs to this task: a failure of the
    /// chosen protocol is reported as that protocol's failure, so the switch is a
    /// decision rather than a preference the window may overrule.
    fn transfer_protocol_choice(
        &self,
        chosen: TransferProtocol,
        metrics: Metrics,
        strings: &'static Strings,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut row = components::segmented(metrics)
            .debug_selector(|| "files.question.protocol".to_owned())
            .child(
                div()
                    .flex_none()
                    .px(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(strings.transfers_protocol)),
            );
        for (protocol, label) in [
            (TransferProtocol::Sftp, strings.transfers_protocol_sftp),
            (TransferProtocol::Scp, strings.transfers_protocol_scp),
        ] {
            let selector = match protocol {
                TransferProtocol::Sftp => "files.question.protocol.sftp",
                TransferProtocol::Scp => "files.question.protocol.scp",
            };
            row = row.child(
                components::segment(label, protocol == chosen, metrics)
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if let Some(dialog) = view.files_dialog.as_mut() {
                            dialog.set_protocol(protocol);
                        }
                        cx.notify();
                    })),
            );
        }
        row
    }

    /// The question about a change, when one is being asked for this session (R11).
    ///
    /// Every variant names the exact object: the full path, and for a deletion also the
    /// kind and whether its contents go with it. Nothing on it sends anything until the
    /// confirming button is pressed.
    fn files_question(
        &self,
        id: SessionId,
        source: FileSource,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let dialog = self
            .files_dialog
            .as_ref()
            .filter(|dialog| dialog.session() == id && dialog.source() == source)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let directory = self
            .files
            .browser_in(id, source)
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
                // A transfer question shows the remote side as a field of its own: a
                // server without an SFTP subsystem has no listing to pick a directory
                // from, and typing the path is what makes SCP possible there at all.
                if let (Some(label), Some(field)) = (
                    match dialog {
                        FilesDialog::Upload { .. } => Some(strings.transfers_remote_directory),
                        FilesDialog::Download { .. } => Some(strings.transfers_remote_path),
                        _ => None,
                    },
                    &self.files_remote_field,
                ) {
                    card = card.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(metrics.scale(2.0))
                            .debug_selector(|| "files.question.remote".to_owned())
                            .child(
                                div()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(SharedString::from(label)),
                            )
                            .child(div().flex_none().child(field.clone())),
                    );
                }
                card = card.child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .debug_selector(|| "files.question.target".to_owned())
                        .child(SharedString::from(display_path(dialog.source(), &target))),
                );
                if let Some(field) = &self.files_field {
                    card = card.child(
                        div()
                            .flex_none()
                            .debug_selector(|| "files.question.input".to_owned())
                            .child(field.clone()),
                    );
                }
                if let Some(chosen) = dialog.protocol() {
                    card = card.child(self.transfer_protocol_choice(chosen, metrics, strings, cx));
                    if chosen == TransferProtocol::Scp {
                        card = card.child(
                            div()
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(palette::TEXT_MUTED))
                                .debug_selector(|| "files.question.protocol.note".to_owned())
                                .child(SharedString::from(strings.transfers_protocol_note)),
                        );
                        card = card.child(
                            components::segment(
                                format!(
                                    "{} {}",
                                    if dialog.recursive() { "[x]" } else { "[ ]" },
                                    strings.transfers_recursive
                                ),
                                dialog.recursive(),
                                metrics,
                            )
                            .id("files.question.recursive")
                            .debug_selector(|| "files.question.recursive".to_owned())
                            .on_click(cx.listener(
                                |view, _event, _window, cx| {
                                    if let Some(dialog) = view.files_dialog.as_mut() {
                                        let next = !dialog.recursive();
                                        dialog.set_recursive(next);
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
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
                            "{}: {}",
                            strings.files_target_label,
                            display_path(dialog.source(), &target)
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
                        .child(SharedString::from(
                            match (dialog.source(), kind.is_directory()) {
                                (FileSource::Local, true) => strings.files_delete_recursive_local,
                                (FileSource::Local, false) => strings.files_delete_file_local,
                                (FileSource::Remote, true) => strings.files_delete_recursive,
                                (FileSource::Remote, false) => strings.files_delete_file,
                            },
                        )),
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
    fn files_report(&self, id: SessionId, source: FileSource) -> Option<Div> {
        let outcome = match source {
            FileSource::Remote => self.files.outcome(id)?,
            FileSource::Local => self.files.outcome_in(id, source)?,
        };
        let done = outcome.succeeded();
        // A change that worked is announced in the corner and needs no room here.
        if done {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let heading = match (source, done) {
            (FileSource::Local, true) => strings.files_change_done_local,
            (FileSource::Local, false) => strings.files_change_failed_local,
            (FileSource::Remote, true) => strings.files_change_done,
            (FileSource::Remote, false) => strings.files_change_failed,
        };
        let report = files_element_id(source, "report");
        let report_heading = files_element_id(source, "report.heading");
        let mut card =
            components::banner(if done { Tone::Positive } else { Tone::Danger }, metrics)
                .gap(metrics.scale(1.0))
                .debug_selector(move || report.to_string())
                .child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(if done {
                            Tone::Positive.text()
                        } else {
                            Tone::Danger.text()
                        })
                        .debug_selector(move || report_heading.to_string())
                        .child(SharedString::from(heading)),
                )
                .child({
                    let count_id = files_element_id(source, "report.count");
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(move || count_id.to_string())
                        .child(SharedString::from(format!(
                            "{} {}/{}",
                            strings.files_change_changed,
                            outcome.changed(),
                            outcome.steps().len()
                        )))
                });
        for (index, step) in outcome.steps().iter().take(FILES_REPORT_STEPS).enumerate() {
            let selector = files_element_id(source, &format!("report.step.{index}"));
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
                    .debug_selector(move || selector.to_string())
                    .child(SharedString::from(line)),
            );
        }
        Some(card)
    }

    /// The row that leads to the parent directory.
    fn files_parent_row(
        &self,
        id: SessionId,
        source: FileSource,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let parent_id = files_element_id(source, "parent");
        let hover = theme::swatch(palette::SURFACE_HOVER);
        div()
            .id(parent_id.clone())
            .debug_selector(move || parent_id.to_string())
            .flex()
            .items_center()
            .flex_none()
            .gap(metrics.scale(spacing::TIGHT))
            .h(metrics.scale(30.0))
            .px(metrics.scale(spacing::SNUG))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |view, _event, _window, cx| {
                view.files_up(id, source, cx);
            }))
            .child(icons::icon(
                Icon::ArrowUp,
                metrics.small_icon_size(),
                theme::swatch(palette::TEXT_MUTED),
            ))
            .child(SharedString::from(format!(
                "..  {}",
                self.strings().files_parent_row
            )))
    }

    /// One entry of the listing on one line: icon, name, size, and its actions.
    ///
    /// A folder opens on click; a file is selected on click and can be dragged to
    /// the other side. Rename, copy to the other side and delete are small icons at
    /// the end of the row, each with its name in a tooltip.
    fn files_row(
        &self,
        id: SessionId,
        source: FileSource,
        index: usize,
        entry: &shellune_core::DirEntry,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let name = entry.name();
        let size = entry.size();
        let kind = entry.kind();
        let openable = entry.is_directory();
        let select_name = name.to_owned();
        let rename = name.to_owned();
        let delete = name.to_owned();
        let download = name.to_owned();
        let transfer_name = name.to_owned();
        let prefix = match source {
            FileSource::Remote => "files",
            FileSource::Local => "files.local",
        };
        let name_selector = format!("{prefix}.name.{index}");
        let entry_selector = format!("{prefix}.entry.{index}");
        let size_text = match size {
            Some(bytes) if !openable => human_size(bytes),
            _ if openable => ux.files_folder.to_owned(),
            _ => kind_label(kind, strings).to_owned(),
        };
        let mut entry_row = div()
            .id(SharedString::from(entry_selector.clone()))
            .debug_selector(move || entry_selector.clone())
            .flex()
            .flex_1()
            .min_w(px(0.0))
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .h_full()
            .child(icons::icon(
                if openable { Icon::Folder } else { Icon::File },
                metrics.small_icon_size(),
                theme::swatch(if openable {
                    palette::ACCENT
                } else {
                    palette::TEXT_MUTED
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .when(openable, |name| name.font_weight(FontWeight::MEDIUM))
                    .debug_selector(move || name_selector.clone())
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .w(metrics.scale(80.0))
                    .truncate()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(SharedString::from(size_text)),
            );
        if openable {
            let open_name = name.to_owned();
            entry_row = entry_row.cursor_pointer().on_click(cx.listener(
                move |view, _event, _window, cx| {
                    view.files_enter(id, source, &open_name, cx);
                },
            ));
        } else if let Some(directory) = self
            .files
            .browser_in(id, source)
            .and_then(|browser| browser.current_directory())
            .map(str::to_owned)
        {
            let drag_name = name.to_owned();
            entry_row = entry_row.on_drag(
                FileDrag {
                    session: id,
                    source,
                    name: drag_name.clone(),
                    directory,
                },
                move |drag, _offset, _window, cx| {
                    cx.new(|_| FileDragLabel {
                        name: SharedString::from(drag.name.clone()),
                    })
                },
            );
        }

        let mut actions = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .w(files_actions_width(metrics, source))
            .gap(metrics.scale(1.0));
        // Only a file has content to copy; a folder button would promise a transfer
        // the connection cannot do. Dragging a file does the same.
        if !openable {
            if source == FileSource::Remote {
                actions = actions
                    .child(self.files_action(
                        SharedString::from(format!("files.download.{index}")),
                        Icon::Download,
                        strings.transfers_download,
                        false,
                        cx,
                        move |view, cx| view.files_ask_download(id, Some(&download), cx),
                    ))
                    .child(self.files_action(
                        SharedString::from(format!("files.to_local.{index}")),
                        Icon::ChevronLeft,
                        strings.files_to_local,
                        false,
                        cx,
                        move |view, cx| {
                            view.files_transfer_named(id, FileSource::Remote, &transfer_name, cx)
                        },
                    ));
            } else {
                actions = actions.child(self.files_action(
                    SharedString::from(format!("files.local.to_remote.{index}")),
                    Icon::ChevronRight,
                    strings.files_to_remote,
                    false,
                    cx,
                    move |view, cx| {
                        view.files_transfer_named(id, FileSource::Local, &transfer_name, cx)
                    },
                ));
            }
        }
        actions = actions
            .child(self.files_action(
                SharedString::from(format!("{prefix}.rename.{index}")),
                Icon::Pencil,
                strings.files_rename,
                false,
                cx,
                move |view, cx| view.files_ask_rename(id, source, &rename, cx),
            ))
            .child(self.files_action(
                SharedString::from(format!("{prefix}.delete.{index}")),
                Icon::Trash,
                strings.files_delete,
                true,
                cx,
                move |view, cx| view.files_ask_delete(id, source, &delete, kind, cx),
            ));

        let hover = theme::swatch(palette::SURFACE_HOVER);
        let row_selector = format!("{prefix}.row.{index}");
        let selected = self.files.selected(id, source) == Some(name);
        // The row's buttons show under the pointer and on the selected row; the
        // rest of the list stays quiet. The same actions are on the right click.
        let row_group = SharedString::from(format!("{prefix}.group.{index}"));
        let actions = if selected {
            actions
        } else {
            actions
                .opacity(0.0)
                .group_hover(row_group.clone(), |style| style.opacity(1.0))
        };
        let menu_entry = files_menu::MenuEntry {
            name: name.to_owned(),
            kind,
            folder: openable,
        };
        div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .group(row_group)
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |view, event: &gpui::MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    view.open_files_menu(id, source, Some(menu_entry.clone()), event.position, cx);
                }),
            )
            .flex()
            .items_center()
            .flex_none()
            .gap(metrics.scale(spacing::TIGHT))
            .h(metrics.scale(30.0))
            .px(metrics.scale(spacing::SNUG))
            .text_size(metrics.label_size())
            .when(selected, |row| row.bg(theme::swatch(palette::ACCENT_TINT)))
            .when(!selected, |row| row.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |view, _event, _window, cx| {
                view.files.select(id, source, Some(select_name.clone()));
                cx.notify();
            }))
            .child(entry_row)
            .child(actions)
    }

    /// A small icon action of a file row.
    fn files_action(
        &self,
        id: SharedString,
        icon: Icon,
        label: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        self.row_icon_button(id, icon, label, danger, cx, click)
            .size(metrics.hit_target())
    }

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

    /// Replaces the remote path of the open transfer question, for a test.
    #[cfg(test)]
    pub fn files_question_set_remote_for_test(&mut self, remote: &str) {
        if let Some(dialog) = self.files_dialog.as_mut() {
            dialog.set_remote(remote.to_owned());
        }
    }

    /// The remote path the open transfer question carries, for a test.
    #[cfg(test)]
    pub fn files_question_remote(&self) -> Option<&str> {
        self.files_dialog.as_ref().and_then(FilesDialog::remote)
    }

    /// The protocol the open transfer question would copy with, for a test.
    #[cfg(test)]
    pub fn files_question_protocol(&self) -> Option<TransferProtocol> {
        self.files_dialog.as_ref().and_then(FilesDialog::protocol)
    }

    /// Whether the open transfer question would copy a whole directory, for a test.
    #[cfg(test)]
    pub fn files_question_recursive(&self) -> bool {
        self.files_dialog
            .as_ref()
            .is_some_and(FilesDialog::recursive)
    }

    /// Why the open transfer question refused the path, for a test.
    #[cfg(test)]
    pub fn files_question_path_problem(&self) -> Option<PathProblem> {
        self.files_dialog
            .as_ref()
            .and_then(FilesDialog::path_problem)
            .copied()
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

    /// The line the transfer center draws for one task.
    #[cfg(test)]
    pub fn transfer_center_text_for_test(&self, id: shellune_core::TransferId) -> Option<String> {
        self.transfers
            .get(id)
            .map(|task| self.transfer_center_text(task))
    }

    /// Whether closing the application is waiting on the user.
    #[cfg(test)]
    pub fn closing_app_for_test(&self) -> bool {
        self.closing_app
    }

    /// Confirms the application close without drawing again afterwards.
    ///
    /// The button removes the window, and a test that clicks it cannot read the
    /// window after that removal. This runs the same confirmation.
    #[cfg(test)]
    pub fn confirm_close_app_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close_app(window, cx);
    }

    /// The line a close dialog draws for one task.
    #[cfg(test)]
    pub fn interrupt_line_for_test(&self, id: shellune_core::TransferId) -> Option<String> {
        self.transfers.get(id).map(|task| self.interrupt_line(task))
    }

    /// The forward lines the close question draws for the session it is asking about.
    #[cfg(test)]
    pub fn closing_forward_lines_for_test(&self) -> Vec<String> {
        let Some(id) = self.closing else {
            return Vec::new();
        };
        self.forwards
            .active_on(id)
            .iter()
            .map(|rule| {
                let target = rule
                    .target()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "SOCKS".to_owned());
                format!("{} · {} → {target}", rule.kind().label(), rule.listen())
            })
            .collect()
    }

    /// The file view of one session, as the window holds it.
    #[cfg(test)]
    pub fn files_browser(&self, id: SessionId) -> Option<&shellune_core::FileBrowser> {
        self.files.browser(id)
    }

    /// The local file view of one session, as the window holds it.
    #[cfg(test)]
    pub fn files_local_browser(&self, id: SessionId) -> Option<&shellune_core::FileBrowser> {
        self.files.browser_in(id, FileSource::Local)
    }

    /// Which source a narrow window is showing.
    #[cfg(test)]
    pub fn files_shown(&self, id: SessionId) -> Option<FileSource> {
        self.files.shown(id)
    }

    /// Replaces the local channel, for a test that scripts this computer.
    #[cfg(test)]
    pub(crate) fn attach_local_files_for_test(
        &mut self,
        id: SessionId,
        channel: Box<dyn BrowseChannel>,
        cx: &mut Context<Self>,
    ) {
        self.files.attach_local_for_test(id, channel);
        cx.notify();
    }

    /// Presses one unmodified key on a file column, or a modified key when `modified` is set.
    ///
    /// The column's own handler is what runs, so the test does not add a window binding.
    #[cfg(test)]
    pub fn files_key_for_test(
        &mut self,
        id: SessionId,
        source: FileSource,
        key: &str,
        modified: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut keystroke = gpui::Keystroke {
            key: key.to_owned(),
            ..gpui::Keystroke::default()
        };
        if modified {
            keystroke.modifiers.control = true;
        }
        self.files_key(
            id,
            source,
            &KeyDownEvent {
                keystroke,
                is_held: false,
            },
            cx,
        )
    }

    /// Drops a file onto one column, using the same request as the row button.
    #[cfg(test)]
    pub fn files_drop_for_test(
        &mut self,
        id: SessionId,
        onto: FileSource,
        from: FileSource,
        directory: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        self.files_accept_drop(
            id,
            onto,
            &FileDrag {
                session: id,
                source: from,
                name: name.to_owned(),
                directory: directory.to_owned(),
            },
            cx,
        );
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
        let selected = self
            .workspace
            .layout()
            .tabs()
            .get(index)
            .is_some_and(|tab| Some(tab.id()) == self.workspace.layout().active_tab_id())
            && self.workspace.destination() == Destination::LocalShells;
        let kind = PaneKind::of(session.kind());
        let tone = session_tone(session.state());
        let state_label = copy::session_state_label(session.state(), self.workspace.locale());
        let live = session.state() == &SessionState::Connected;
        let kind_label = match kind {
            PaneKind::Local => strings.session_kind_local,
            PaneKind::Remote => strings.session_kind_remote,
        };
        let jump = if index < crate::app::TAB_JUMP_COUNT {
            keymap::tab_jump_action(index + 1)
                .and_then(|spec| self.keymap.label(spec.id))
                .map(|label| format!(" ({label})"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        // What this tab is carrying from the background (R7): a bell or a command end,
        // in words, so the mark is read and not only coloured.
        let unread = self.workspace.session_unread(id);
        let tooltip = format!(
            "{} · {kind_label} · {} · {state_label}{jump}{marked}",
            self.tab_title(id),
            self.target_of(session),
            marked = tab_attention_summary(unread, strings)
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
                    this.request_close_tab_anchor(id, cx);
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
            .children(
                self.workspace
                    .layout()
                    .tabs()
                    .get(index)
                    .and_then(|tab| tab.accent())
                    .map(|accent| {
                        tab_accent_mark(accent, metrics, format!("session.tab.accent.{index}"))
                    }),
            )
            .child(icons::icon(
                if kind == PaneKind::Remote {
                    Icon::Server
                } else {
                    Icon::Terminal
                },
                metrics.scale(13.0),
                theme::swatch(if selected {
                    palette::ACCENT
                } else {
                    palette::TEXT_MUTED
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .when(selected, |label| label.font_weight(FontWeight::MEDIUM))
                    .child(SharedString::from(self.tab_title(id))),
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
            // The background marks sit inside the tab's own target, so choosing the
            // mark is choosing the tab: it shows that tab and its pane, which is also
            // what clears the mark (R7).
            .children(tab_attention_marks(unread, strings, metrics, index))
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    // A pointer target, not a 20px drawing: the smallest size R4
                    // allows, so closing a tab never needs a careful aim.
                    .w(metrics.hit_target())
                    .h(metrics.hit_target())
                    .id(("session.tab.close", index))
                    .debug_selector({
                        let selector = format!("session.tab.close.{index}");
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.stop_propagation();
                        this.request_close_tab_anchor(id, cx);
                    })),
            )
    }

    /// The label of one tab: the user's own name, or the session's title numbered
    /// when an earlier tab has the same one, so two shells of one program can be told
    /// apart at a glance.
    ///
    /// A name the user typed is shown exactly as typed: numbering it would alter what
    /// they wrote. Two tabs with one name are still told apart by the target, the kind
    /// and the state, which the tooltip and the tab list both write out (R2).
    fn tab_title(&self, id: SessionId) -> String {
        let custom = self
            .workspace
            .tab_of_session(id)
            .and_then(|tab| self.workspace.tab_custom_title(tab));
        if let Some(custom) = custom {
            return custom.to_owned();
        }
        let Some(title) = self.workspace.session(id).map(Session::title) else {
            return String::new();
        };
        let tabs = self.tab_sessions();
        let index = tabs.iter().position(|tab| *tab == id).unwrap_or(0);
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

    /// The label the tab strip shows for one session (R2), for the tests that name a
    /// tab and then check what the strip really says.
    #[cfg(test)]
    pub fn tab_label_for_test(&self, id: SessionId) -> String {
        self.tab_title(id)
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
        for index in 0..self.workspace.layout().tabs().len() {
            if let Some(id) = self.workspace.layout().anchor_of(index) {
                row = row.child(self.session_tab(id, index, cx));
            } else {
                row = row.child(self.pending_tab(index, cx));
            }
        }
        row
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
        // A live session is the normal case, and its tab already shows the green
        // dot; the bar only speaks up while connecting, or once it failed or ended.
        let live = matches!(session.state(), SessionState::Connected);
        let target = match session.kind() {
            SessionKind::Local { .. } => program_label(&self.target_of(session)),
            SessionKind::Remote { .. } => self.target_of(session),
        };
        // A local shell's program is often its own title; saying it twice is noise.
        let label = self.tab_title(id);
        let target = (target.to_lowercase() != label.to_lowercase()).then_some(target);
        let state_label = copy::session_state_label(session.state(), self.workspace.locale());
        let tone = session_tone(session.state());
        let can_reconnect = may_reconnect(session);
        // Only a connection that can really browse offers the file view: a local
        // session has no server to ask, and a session whose connection ended has no
        // directory left to show.
        let browsable = self.files.can_browse(id);
        // A saved host's tunnels are one click away from its terminal.
        let forward_host = match session.kind() {
            SessionKind::Remote { host } if self.book.host(*host).is_some() => Some(*host),
            _ => None,
        };
        let files_open = self.files.is_open(id);
        // Terminal and files are two views of the same connection: a switch, not a
        // pair of open and close buttons.
        let files_button = browsable.then(|| {
            let ux = copy::ux();
            let view_segment =
                |selector: &'static str, icon: Icon, label: &'static str, selected: bool| {
                    let color = theme::swatch(if selected {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_SECONDARY
                    });
                    components::segment("", selected, metrics)
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE + 1.0))
                        .child(icons::icon(icon, metrics.small_icon_size(), color))
                        .child(label)
                        .id(selector)
                        .debug_selector(move || selector.to_owned())
                };
            components::segmented(metrics)
                .child(
                    view_segment(
                        "files.close",
                        Icon::Terminal,
                        ux.settings_terminal,
                        !files_open,
                    )
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if view.files.is_open(id) {
                            view.toggle_files(id, cx);
                        }
                    })),
                )
                .child(
                    view_segment(
                        "session.files",
                        Icon::Folder,
                        strings.files_open,
                        files_open,
                    )
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if !view.files.is_open(id) {
                            view.toggle_files(id, cx);
                        }
                    })),
                )
        });

        let row =
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
                        .child(
                            div()
                                .id("session.kind")
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(metrics.scale(22.0))
                                .rounded(px(radius::SMALL))
                                .bg(theme::swatch(palette::ACCENT_TINT))
                                .tooltip(components::tooltip(kind_label))
                                .child(icons::icon(
                                    if kind == PaneKind::Remote {
                                        Icon::Server
                                    } else {
                                        Icon::Terminal
                                    },
                                    metrics.small_icon_size(),
                                    theme::swatch(palette::ACCENT),
                                )),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(metrics.scale(240.0))
                                .truncate()
                                .text_size(metrics.label_size())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                .debug_selector(|| "session.identity".to_owned())
                                // "Current" only tells panes apart; a lone pane is
                                // simply the session.
                                .child(SharedString::from(if self.active_tab_is_solo() {
                                    label.clone()
                                } else {
                                    format!("{} · {label}", strings.workspace_current)
                                })),
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
                        .when(!live, |row| {
                            row.child(components::chip(state_label, tone, metrics))
                        })
                        .children(self.unsaved.iter().find(|(session, _)| *session == id).map(
                            |_| {
                                div()
                                    .flex_none()
                                    .truncate()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED))
                                    .child(strings.quick_unsaved_note)
                            },
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .children(self.solo_split_buttons(cx))
                        .when_some(files_button, |row, button| row.child(button))
                        .when_some(forward_host, |row, host| {
                            row.child(self.icon_text_button(
                                SharedString::from("session.forwards"),
                                Icon::Forward,
                                copy::ux().session_forwards,
                                Variant::Ghost,
                                cx,
                                move |view, cx| {
                                    view.show_forwards_scope(
                                        crate::app::forwarding::ForwardScope::Host(host),
                                        cx,
                                    );
                                },
                            ))
                        })
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
                        .child(
                            components::icon_only_button(Icon::Close, metrics)
                                .id("session.close")
                                .debug_selector(|| "session.close".to_owned())
                                .tooltip(components::tooltip(strings.session_close))
                                .on_click(cx.listener(move |view, _event, _window, cx| {
                                    view.request_close_session(id, cx);
                                })),
                        ),
                );
        let notice = self.quick_save_notice.clone();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .child(row)
            .children(notice.map(|notice| self.quick_save_banner(&notice)))
    }

    /// The save result, on the session bar that holds the save button.
    fn quick_save_banner(&self, notice: &QuickSaveNotice) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let tone = if notice.failed {
            Tone::Danger
        } else {
            Tone::Positive
        };
        components::banner(tone, metrics)
            .debug_selector(|| "session.save.notice".to_owned())
            .child(
                div()
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "session.save.reason".to_owned())
                    .child(SharedString::from(notice.reason.clone())),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .debug_selector(|| "session.save.next".to_owned())
                    .child(SharedString::from(format!(
                        "{}: {}",
                        strings.next_step, notice.next
                    ))),
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

    fn transfer_rows(&self, mut list: Div, cx: &mut Context<Self>) -> Div {
        for (index, transfer) in self.workspace.transfers().items().iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.transfer_row(index, transfer, cx)),
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
            _ => self.target_of(session),
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

    fn transfer_row(&self, index: usize, transfer: &Transfer, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let tone = Tone::for_transfer_state(transfer.state());
        let summary = self.transfer_center_text(transfer);
        let actions = self.transfer_actions(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::HAIRLINE)),
            "transfers.center",
            index,
            transfer,
            true,
            cx,
        );
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::HAIRLINE))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::SNUG))
                            .min_w(px(0.0))
                            .child(components::status_dot(tone, metrics))
                            .child(components::kind_badge(
                                self.direction_label(transfer.direction()),
                                false,
                                metrics,
                            ))
                            .child(components::stacked(
                                transfer.source().to_owned(),
                                // Host, target, progress and residue, from the same task
                                // the file view reads. Leaving that view does not change
                                // the numbers.
                                format!(
                                    "{} {} · \u{2192} {} · {}",
                                    strings.transfers_host,
                                    self.transfer_host_label(transfer),
                                    transfer.destination(),
                                    self.transfer_detail(transfer)
                                ),
                                metrics,
                            )),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("transfers.center.summary.{index}"))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(summary)),
                    )
                    .child(actions),
            )
            .child(components::chip(
                copy::transfer_state_label(transfer.state(), locale),
                tone,
                metrics,
            ))
    }

    /// Direction, host, source, target and the same detail the file view draws.
    fn transfer_center_text(&self, transfer: &Transfer) -> String {
        format!(
            "{} · {} {} · {} → {} · {}",
            self.direction_label(transfer.direction()),
            self.strings().transfers_host,
            self.transfer_host_label(transfer),
            transfer.source(),
            transfer.destination(),
            self.transfer_detail(transfer)
        )
    }

    /// Switches the interface language, and tells every open terminal about it.
    ///
    /// Two paths reach this: the shortcut, which only applies outside a terminal, and
    /// the language control in the status bar, which is how the language is switched
    /// while a terminal has the keyboard (there Ctrl+L belongs to the shell).
    fn toggle_locale(&mut self, cx: &mut Context<Self>) {
        self.workspace.toggle_locale();
        self.relabel_fields(cx);
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
        self.persist_preferences();
        if self.focus_lost_watch.is_none() {
            self.focus_lost_watch = Some(cx.on_focus_lost(window, |view, window, cx| {
                view.reclaim_focus(window, cx);
            }));
        }
        self.viewport_width = window.viewport_size().width;
        // The palette is decided before anything is drawn: the user's choice, or the
        // system's when the choice is to follow it, which the window reports when it
        // changes.
        if !self.watching_close {
            self.watching_close = true;
            let view = cx.entity().downgrade();
            window.on_window_should_close(cx, move |_window, cx| {
                view.update(cx, |view, cx| view.refuse_app_close(cx))
                    .unwrap_or(true)
            });
        }
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
        let appearance = self.theme_choice.appearance(system_dark);
        theme::set_appearance(appearance);
        theme::set_terminal_appearance(self.terminal_scheme.appearance(appearance));
        theme::set_motion(self.workspace.motion());
        // A field's focus is noted before a click can blur it. Opening an overlay then
        // still knows which field to restore, even when the click already moved focus.
        self.note_content_focus(window, cx);
        // A closed overlay asked to put the keyboard back where it was opened from.
        // Otherwise a panel that closed while one of its fields had the keyboard leaves
        // nothing focused, and every shortcut would go nowhere; the page takes it back.
        if let Some(id) = self.pane_focus_pending.take()
            && self.active == Some(id)
            && self.shows_terminal(self.workspace.destination())
            && let Some(pane) = self.pane_for(id)
        {
            let handle = pane.read(cx).focus_handle().clone();
            if !handle.is_focused(window) {
                window.focus(&handle);
            }
        }
        if std::mem::take(&mut self.restore_focus_pending) {
            self.restore_overlay_focus(window, cx);
        } else if window.focused(cx).is_none() {
            let destination = self.workspace.destination();
            let handle = self.destination_focus(destination).clone();
            window.focus(&handle);
        }
        if std::mem::take(&mut self.focus_host_dialog) {
            let handle = self
                .host_fields
                .as_ref()
                .map(|fields| fields.name.read(cx).focus_handle().clone());
            if let Some(handle) = handle {
                self.remember_focus(window, cx);
                window.focus(&handle);
            }
        }
        // A command editor that was just opened takes the keyboard in its first field, so
        // the dialog can be written without touching the pointer.
        if let Some(handle) = self.snippet_focus_pending.take()
            && !handle.is_focused(window)
        {
            window.focus(&handle);
        }
        let metrics = self.metrics();
        let notice = self.notice_strip(cx);

        div()
            .id("shellune.root")
            .relative()
            .flex()
            .size_full()
            .bg(theme::swatch(palette::CANVAS))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .key_context("Shellune")
            .track_focus(&self.root_focus)
            .capture_any_mouse_down(cx.listener(|this, _event, _window, _cx| {
                this.pointer_input = true;
            }))
            .capture_key_down(cx.listener(|this, _event, _window, cx| {
                if this.pointer_input {
                    this.pointer_input = false;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &GoToLocalShells, window, cx| {
                this.select(Destination::LocalShells, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToHosts, window, cx| {
                this.select(Destination::Hosts, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToSettings, window, cx| {
                this.select(Destination::Settings, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToSessions, window, cx| {
                this.select(Destination::Sessions, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToTransfers, window, cx| {
                this.select(Destination::Transfers, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToForwards, window, cx| {
                this.open_global_forwards(window, cx);
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
                if this.custom_shell_open {
                    this.close_custom_shell(window, cx);
                    return;
                }
                if this.host_fields.is_some() {
                    this.close_host_editor(cx);
                    return;
                }
                if this.tab_rename.is_some() {
                    this.close_tab_rename(window, cx);
                    return;
                }
                // The shortcut-command manager gives up one layer at a time: the editor, then
                // the deletion question, then the dialog itself (R6).
                if this.snippet_panel.is_editing() {
                    this.close_snippet_editor(cx);
                    return;
                }
                if this.snippet_panel.pending_delete().is_some() {
                    this.snippet_panel.keep();
                    cx.notify();
                    return;
                }
                if this.tool == Some(shell::Tool::Snippets) {
                    this.tool = None;
                    this.restore_focus_pending = true;
                    cx.notify();
                    return;
                }
                if this.tab_list_open {
                    this.close_tab_list(window, cx);
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
            .on_action(cx.listener(|this, _: &SplitPaneRight, window, cx| {
                this.split_active(SplitAxis::LeftRight, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitPaneDown, window, cx| {
                this.split_active(SplitAxis::TopBottom, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomPane, _window, cx| {
                this.toggle_active_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| {
                this.focus_direction(crate::app::layout::Direction::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| {
                this.focus_direction(crate::app::layout::Direction::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| {
                this.focus_direction(crate::app::layout::Direction::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| {
                this.focus_direction(crate::app::layout::Direction::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneLeft, window, cx| {
                this.resize_direction(crate::app::layout::Direction::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneRight, window, cx| {
                this.resize_direction(crate::app::layout::Direction::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneUp, window, cx| {
                this.resize_direction(crate::app::layout::Direction::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneDown, window, cx| {
                this.resize_direction(crate::app::layout::Direction::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneLeft, window, cx| {
                this.move_direction(crate::app::layout::Direction::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneRight, window, cx| {
                this.move_direction(crate::app::layout::Direction::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneUp, window, cx| {
                this.move_direction(crate::app::layout::Direction::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneDown, window, cx| {
                this.move_direction(crate::app::layout::Direction::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveTabLeft, _window, cx| {
                this.reorder_active_tab(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveTabRight, _window, cx| {
                this.reorder_active_tab(1, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectTab9, window, cx| {
                let last = this.workspace.layout().tabs().len().saturating_sub(1);
                this.select_tab(last, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(self.top_bar(window, cx))
                    .children(notice)
                    .children(self.close_transfer_question(cx))
                    .children(self.close_app_question(cx))
                    .child(self.destination_panel(cx)),
            )
            // Floating layers, drawn after the page and so above it, the later the
            // higher. They are plain layers rather than deferred draws, so a field
            // inside one can still defer its own right-click menu: gpui refuses a
            // deferred draw inside another.
            .children(self.connection_overlay(window, cx))
            .children(self.import_overlay(window, cx))
            .children(self.tool_overlay(window, cx))
            .children(self.host_dialog_overlay(window, cx))
            .children(self.custom_shell_overlay(window, cx))
            .children(self.shell_menu_overlay(window, cx))
            .children(self.files_menu_overlay(window, cx))
            .children(self.tab_menu_overlay(window, cx))
            .children(self.tab_rename_overlay(window, cx))
            .children(self.tab_list_overlay(window, cx))
            .children(self.session_menu_overlay(window, cx))
            .children(self.palette_overlay(window, cx))
            .children(self.key_capture_overlay(window, cx))
            .children(self.toast_stack(cx))
    }
}

/// A window-sized layer at the top-left of the root, for a dialog or a menu.
fn overlay_layer() -> Div {
    div().absolute().top_0().left_0()
}

/// The small mark that gives a tab its own colour, when it has one.
///
/// It is drawn beside the status dot and never instead of it: the colour says which
/// tab this is, while the dot and the state word say what the session is doing. A tab
/// without a colour draws nothing here, so no state depends on colour being set (R2).
fn tab_accent_mark(accent: TabAccent, metrics: Metrics, selector: String) -> Div {
    div()
        .flex_none()
        .w(metrics.scale(3.0))
        .h(metrics.scale(16.0))
        .rounded_full()
        .bg(theme::tab_accent_color(accent))
        .debug_selector(move || selector.clone())
}

/// The background marks one tab carries until it is looked at (R7).
///
/// Each mark is a word: "Bell" and "Ended" say what happened without a colour and
/// without an icon the reader has to learn. Nothing here animates, so switching reduced
/// motion has nothing to suppress and a mark never blinks. The mark carries no command
/// line, output or credential — the terminal only reports that one of the two events
/// happened.
fn tab_attention_marks(
    unread: UnreadNotices,
    strings: &Strings,
    metrics: Metrics,
    index: usize,
) -> Vec<Div> {
    let mut marks = Vec::new();
    let kinds = [
        (unread.bell, strings.tab_mark_bell, "bell"),
        (unread.finished, strings.tab_mark_finished, "finished"),
    ];
    for (present, label, name) in kinds {
        if !present {
            continue;
        }
        let selector = format!("session.tab.mark.{name}.{index}");
        marks.push(
            div()
                .flex_none()
                .text_size(metrics.caption_size())
                .font_weight(FontWeight::MEDIUM)
                // The accent says "new", never which kind: the word already does, and a
                // status hue here would compete with the connection state beside it.
                .text_color(Tone::Accent.text())
                .debug_selector(move || selector.clone())
                .child(SharedString::from(label)),
        );
    }
    marks
}

/// The words a tab's tooltip adds for the marks it is carrying (R7).
fn tab_attention_summary(unread: UnreadNotices, strings: &Strings) -> String {
    let mut parts: Vec<&'static str> = Vec::new();
    if unread.bell {
        parts.push(strings.tab_mark_bell);
    }
    if unread.finished {
        parts.push(strings.tab_mark_finished);
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", parts.join(" · "))
    }
}

/// Why a port the quick form cannot dial, naming the field to change.
fn quick_port_reason(entered: &str, strings: &Strings) -> String {
    let shown = if entered.is_empty() {
        strings.quick_port_blank
    } else {
        entered
    };
    format!(
        "{} ({}). {}",
        strings.quick_port_invalid, shown, strings.quick_fix_port
    )
}

/// Why [`HostConfig::new`] refused the quick form, in the current language.
///
/// The domain error's English text is not shown: each variant names the field.
fn quick_host_reason(error: &HostValidationError, strings: &Strings) -> String {
    match error {
        HostValidationError::EmptyName => strings.quick_invalid_name.to_owned(),
        HostValidationError::EmptyAddress => strings.quick_invalid_address.to_owned(),
        HostValidationError::AddressContainsWhitespace => {
            strings.quick_invalid_address_space.to_owned()
        }
        HostValidationError::EmptyUser => strings.quick_invalid_user.to_owned(),
        HostValidationError::PortOutOfRange { value } => {
            format!(
                "{} ({}). {}",
                strings.quick_port_invalid, value, strings.quick_fix_port
            )
        }
    }
}

/// The secret the chosen method still lacks, or nothing when it can dial.
fn quick_auth_gap(
    method: MethodChoice,
    password: &str,
    key_path: &str,
    passphrase: &str,
    strings: &Strings,
) -> Option<String> {
    match method {
        MethodChoice::Password if password.is_empty() => {
            Some(strings.auth_missing_password.to_owned())
        }
        MethodChoice::KeyFile | MethodChoice::EncryptedKeyFile if key_path.is_empty() => {
            Some(strings.auth_missing_key.to_owned())
        }
        MethodChoice::EncryptedKeyFile if passphrase.is_empty() => {
            Some(strings.auth_missing_passphrase.to_owned())
        }
        _ => None,
    }
}

/// Records the method on the unsaved host. A key path is not a secret; a password
/// or passphrase is never written here.
fn apply_quick_auth(host: &mut HostConfig, method: MethodChoice, key_path: &str) {
    host.set_auth(Some(method.kind()));
    if method.needs_key_path() {
        host.set_identity_file(Some(key_path));
    }
}

/// Localized reason and next step when the host book refuses the save.
fn quick_book_failure(error: &BookError, strings: &Strings) -> (String, String) {
    match error {
        BookError::DuplicateHost { .. } => (
            strings.quick_save_duplicate.to_owned(),
            strings.quick_save_duplicate_next.to_owned(),
        ),
        BookError::UnknownHost { .. } => (
            strings.quick_save_unknown.to_owned(),
            strings.quick_save_unknown_next.to_owned(),
        ),
        BookError::Inconsistent { .. } => (
            strings.quick_save_inconsistent.to_owned(),
            strings.quick_save_inconsistent_next.to_owned(),
        ),
    }
}

/// Localized reason and next step when the host file cannot be written.
fn quick_storage_failure(error: &StorageError, strings: &Strings) -> (String, String) {
    let reason = match error.path() {
        Some(path) => format!("{} ({})", strings.quick_save_write_failed, path.display()),
        None => strings.quick_save_write_failed.to_owned(),
    };
    (reason, strings.quick_save_write_next.to_owned())
}

/// Debug selector of one quick-connection method, distinct from the remote form.
fn quick_method_selector(choice: MethodChoice) -> &'static str {
    match choice {
        MethodChoice::Password => "quick.method.password",
        MethodChoice::KeyFile => "quick.method.key",
        MethodChoice::EncryptedKeyFile => "quick.method.key-passphrase",
        MethodChoice::Agent => "quick.method.agent",
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

/// A text size as the percent the preferences file records.
fn zoom_percent(zoom: theme::Zoom) -> u16 {
    match zoom {
        theme::Zoom::Compact => 90,
        theme::Zoom::Regular => 100,
        theme::Zoom::Comfortable => 115,
        theme::Zoom::Largest => 200,
    }
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

/// A byte count as people read it: 12 B, 3.4 KB, 120 MB.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// The words and tone of a connection's state.
fn remote_status_label(status: &RemoteStatus, strings: &Strings) -> (&'static str, Tone) {
    match status {
        RemoteStatus::Idle => (strings.remote_idle, Tone::Neutral),
        RemoteStatus::Connecting => (strings.remote_connecting, Tone::Warning),
        RemoteStatus::VerifyingHost(_) => (strings.remote_verifying, Tone::Warning),
        RemoteStatus::ConfirmingProxy { .. } => (strings.remote_proxy_title, Tone::Warning),
        RemoteStatus::Live => (strings.remote_live, Tone::Positive),
        RemoteStatus::Failed(_) => (strings.remote_failed, Tone::Danger),
    }
}

/// The icon a destination is drawn with in the sidebar.
fn destination_icon(destination: Destination) -> Icon {
    match destination {
        Destination::LocalShells => Icon::Terminal,
        Destination::Hosts => Icon::Server,
        Destination::Sessions => Icon::Layers,
        Destination::Transfers => Icon::Transfer,
        Destination::Forwards => Icon::Forward,
        Destination::Settings => Icon::Settings,
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
