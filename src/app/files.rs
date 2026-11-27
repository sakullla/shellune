//! The file view of one remote session: a local directory and that session's SFTP
//! directory, each with its own path, request number and listing (R8).
//!
//! The state machine that decides what a directory listing may say lives in
//! shellune-core, because it has to be testable without a window. What lives here is
//! the wiring. A session keeps two browsers and nothing is shared with another session:
//!
//! * the link to that session's own connection, so the remote side never re-authenticates;
//! * a local filesystem channel that answers the same requests, so there is no second protocol;
//! * each browser's path, request number and listing, plus the host the remote path belongs to.
//!
//! The view asks for entries only after a real answer: pump() applies what came back
//! and reports whether anything changed, and a request that could not even be
//! delivered is turned into a connection failure for that one source. A failure of one
//! source does not clear the other, and neither becomes a failure of the terminal.
//!
//! A change is answered by a report, then the same source is listed again. The report
//! says what happened; only the next listing says what the directory holds now.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use shellune_core::{
    BrowseFailure, BrowseRequest, BrowseResponse, BrowseState, DirEntry, FileBrowser, FileKind,
    MutationAction, MutationOutcome, MutationStep, NameProblem, SessionId, TransferProtocol,
    join_path, normalize_path, parent_path,
};
use shellune_ssh::FileLink;

use crate::app::copy::Strings;
use crate::app::transfers::{PathProblem, TransferProblem};

/// Where a file view sends its requests and reads the answers.
///
/// The real implementation is the session's connection ([FileLink]); a test supplies
/// its own, which is what makes the view's behaviour observable without a server.
pub trait BrowseChannel {
    /// Asks for something, returning whether the request left this process.
    fn send(&mut self, request: BrowseRequest) -> bool;
    /// Every answer that arrived since the last call.
    fn drain(&mut self) -> Vec<BrowseResponse>;
}

impl BrowseChannel for FileLink {
    fn send(&mut self, request: BrowseRequest) -> bool {
        FileLink::send(self, request)
    }

    fn drain(&mut self) -> Vec<BrowseResponse> {
        FileLink::drain(self)
    }
}

/// Which directory a file view is asking about.
///
/// Local and remote stay separate values: a narrow window may show only one of them,
/// but it never folds the two into a single identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileSource {
    /// A directory on this computer.
    Local,
    /// A directory on the session's own SSH connection.
    Remote,
}

/// One control of the file view the keyboard can stop on, in the order the page
/// draws them (R4).
///
/// The window does not hand out a collection-wide tab order: the column that holds the
/// keyboard keeps it and offers its own controls, and each of them is a stop here. The
/// list is what `files_key` steps through, so a failed read of a server without an SFTP
/// subsystem really does reach its two typed-path entries with the keyboard alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilesStop {
    /// The column itself: the listing, or the card that says why there is none. The
    /// keyboard arrives here and Page Up/Down turns what it holds.
    Body,
    /// The toolbar's upload entry (the typed local path).
    ToolbarUpload,
    /// The toolbar's download entry, drawn where there is no listing to pick a row from.
    ToolbarDownload,
    /// The failed read's typed-path upload entry.
    CardUpload,
    /// The failed read's typed-path download entry.
    CardDownload,
    /// The question's local path field.
    QuestionLocal,
    /// The question's remote path field.
    QuestionRemote,
    /// The question's SFTP choice.
    QuestionSftp,
    /// The question's SCP choice.
    QuestionScp,
    /// The question's recursive switch, drawn once SCP is the chosen protocol.
    QuestionRecursive,
    /// The question's confirming button.
    QuestionConfirm,
    /// The question's cancelling button.
    QuestionCancel,
    /// The field that filters one column's names (R14).
    ///
    /// It is a stop of its own so a column can be filtered with the keyboard alone:
    /// Tab reaches the field, typing narrows the list, and the keys it does not use
    /// step back out to the column.
    Filter,
    /// The switch that makes the remote column follow the shell's directory (R15).
    ///
    /// It is a stop of its own so following can be turned off or taken back without a
    /// pointer, which matters when the reported directory is one this account may not
    /// read: the switch is how the user takes over.
    Follow,
    /// The transfer strip: the failure and its retry or cancel. Page Up/Down turn the
    /// strip when the row is taller than the slice the window keeps for it (R4).
    Transfers,
    /// The session bar's switch back to the terminal, which is how the view is left: the
    /// file view takes the terminal's place, so the way back has to be reachable with the
    /// keyboard as well as by Escape (R4).
    Close,
}

/// What the follow switch of one session is doing (R15).
///
/// The state is per session and per directory report, so two servers on two
/// connections cannot describe each other's directories, and a file view says why it
/// is not following instead of showing a path nobody reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FollowNote {
    /// Following, and the shell of this session has not stated where it is yet.
    ///
    /// The listing keeps whatever the server really returned; it is not relabelled as
    /// the login directory and it is not filled from ordinary output.
    NoReport,
    /// Following, and the listing shows where the shell said it is.
    Following { path: String },
    /// Following was turned off because the user browsed somewhere by hand.
    Manual,
    /// The reported directory could not be read, so the last readable one is shown.
    Unreadable { path: String },
}

/// What one column orders its listing by (R14).
///
/// Ordering and filtering are the column's own: a name sorted on this computer says
/// nothing about the server's column, and a filter typed into one of them must not
/// hide rows of the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// By name, ignoring case; the default.
    Name,
    /// By the size the server reported.
    Size,
    /// By when the entry last changed.
    Modified,
}

/// How one column orders its listing: a key and a direction.
///
/// Folders stay first whichever way the column is sorted, because a listing is a
/// workspace first: the folder a reader is about to open is not ranked against the
/// files beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSort {
    key: SortKey,
    descending: bool,
}

impl FileSort {
    /// The default of every column: names, A to Z.
    #[must_use]
    pub fn name() -> Self {
        Self {
            key: SortKey::Name,
            descending: false,
        }
    }

    /// What the column orders by.
    #[must_use]
    pub fn key(self) -> SortKey {
        self.key
    }

    /// Whether the order runs backwards.
    #[must_use]
    pub fn descending(self) -> bool {
        self.descending
    }

    /// The order a header press asks for: this key ascending, or the other way round
    /// when the column is already sorted by it.
    #[must_use]
    pub fn pressed(self, key: SortKey) -> Self {
        if self.key == key {
            Self {
                key,
                descending: !self.descending,
            }
        } else {
            // A time is asked for to find what changed last, so it starts newest first;
            // a name or a size starts at the small end.
            Self {
                key,
                descending: key == SortKey::Modified,
            }
        }
    }
}

/// One browsable session: the remote connection, the local disk, and both views.
struct SessionFiles {
    remote: Box<dyn BrowseChannel>,
    local: Box<dyn BrowseChannel>,
    /// None until the user opens the file view for this session.
    remote_browser: Option<FileBrowser>,
    local_browser: Option<FileBrowser>,
    host: String,
    target: String,
    /// Which source a narrow window shows. A wide window shows both.
    shown: FileSource,
    local_selected: Option<String>,
    remote_selected: Option<String>,
    /// Whether the remote column follows this session's own shell (R15).
    follow: bool,
    /// The last directory the shell of this session stated, usable or not tried yet.
    ///
    /// Only OSC 7 puts a value here: output that looks like a path never does.
    reported: Option<String>,
    /// The last remote directory the server really listed.
    ///
    /// It is where following returns to when a reported directory cannot be read, so a
    /// refused path does not leave the column showing nothing.
    last_good: Option<String>,
    /// The directory a follow request is waiting for, so its answer can be told apart
    /// from an answer to a request the user made.
    follow_target: Option<String>,
    /// What is said beside the follow switch.
    note: FollowNote,
    /// Whether a directory report arrived while this session was not the active pane
    /// and still has to be followed once it is (R15).
    ///
    /// The report is kept either way; this only records that the listing and the note
    /// are behind it, so the column cannot claim to follow a directory it is not showing.
    deferred_follow: bool,
    /// How the local column orders its own listing (R14).
    local_sort: FileSort,
    /// How the remote column orders its own listing (R14).
    remote_sort: FileSort,
    /// The text the local column keeps only names containing (R14).
    local_filter: String,
    /// The text the remote column keeps only names containing (R14).
    remote_filter: String,
    /// Whether the local column includes dotfiles (names starting with `.`).
    local_show_hidden: bool,
    /// Whether the remote column includes dotfiles (names starting with `.`).
    remote_show_hidden: bool,
    /// The local entries the user marked, in the order they were marked (R14).
    ///
    /// The mark is separate from the row the keyboard is on: one row is where typing
    /// and opening happen, and the marks are what a batch action acts on.
    local_marked: Vec<String>,
    /// The remote entries the user marked, in the order they were marked (R14).
    remote_marked: Vec<String>,
    /// Local deletions still to send, one at a time (R14).
    ///
    /// A browser answers one request at a time, so a batch is a queue rather than
    /// several requests at once: each deletion is sent when the previous one and its
    /// listing have been answered.
    local_pending: VecDeque<String>,
    /// Remote deletions still to send, one at a time (R14).
    remote_pending: VecDeque<String>,
}

/// Every browsable session and which of them show a file view.
#[derive(Default)]
pub struct FileViews {
    sessions: HashMap<SessionId, SessionFiles>,
}

impl FileViews {
    /// No file view, and no browsable session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a session's connection can browse directories.
    ///
    /// The host and address are kept with the link so a view can never show a path
    /// without naming the machine it is on. The local side is this computer; it does
    /// not use the session's credentials.
    pub fn attach(
        &mut self,
        session: SessionId,
        host: impl Into<String>,
        target: impl Into<String>,
        channel: Box<dyn BrowseChannel>,
    ) {
        self.sessions.insert(
            session,
            SessionFiles {
                remote: channel,
                local: Box::new(LocalFs::new()),
                remote_browser: None,
                local_browser: None,
                host: host.into(),
                target: target.into(),
                shown: FileSource::Remote,
                local_selected: None,
                remote_selected: None,
                // Following is what a file view is for: it starts on, and a shell that
                // has said nothing leaves the note saying so (R15).
                follow: true,
                reported: None,
                last_good: None,
                follow_target: None,
                note: FollowNote::NoReport,
                deferred_follow: false,
                local_sort: FileSort::name(),
                remote_sort: FileSort::name(),
                local_filter: String::new(),
                remote_filter: String::new(),
                local_show_hidden: true,
                remote_show_hidden: true,
                local_marked: Vec::new(),
                remote_marked: Vec::new(),
                local_pending: VecDeque::new(),
                remote_pending: VecDeque::new(),
            },
        );
    }

    /// Replaces the local channel, for a test or a capture that scripts this computer.
    ///
    /// The remote channel is left as it was: a script for one source must not become
    /// the other source's answers.
    #[cfg(any(test, feature = "ui-capture"))]
    pub fn attach_local_for_test(&mut self, session: SessionId, channel: Box<dyn BrowseChannel>) {
        if let Some(files) = self.sessions.get_mut(&session) {
            files.local = channel;
        }
    }

    /// Whether this session has a connection that can browse directories.
    #[must_use]
    pub fn can_browse(&self, session: SessionId) -> bool {
        self.sessions.contains_key(&session)
    }

    /// Whether the file view of a session is open.
    #[must_use]
    pub fn is_open(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|files| files.remote_browser.is_some())
    }

    /// Whether any file view is open, which is when polling is worth its timer.
    #[must_use]
    pub fn any_open(&self) -> bool {
        self.sessions
            .values()
            .any(|files| files.remote_browser.is_some())
    }

    /// The remote view of one session, when it is open.
    #[must_use]
    pub fn browser(&self, session: SessionId) -> Option<&FileBrowser> {
        self.browser_in(session, FileSource::Remote)
    }

    /// The view of one source, when the file view is open.
    #[must_use]
    pub fn browser_in(&self, session: SessionId, source: FileSource) -> Option<&FileBrowser> {
        let files = self.sessions.get(&session)?;
        match source {
            FileSource::Local => files.local_browser.as_ref(),
            FileSource::Remote => files.remote_browser.as_ref(),
        }
    }

    /// What the last remote change really did, when one was attempted.
    #[must_use]
    pub fn outcome(&self, session: SessionId) -> Option<&MutationOutcome> {
        self.outcome_in(session, FileSource::Remote)
    }

    /// What the last change of one source really did.
    #[must_use]
    pub fn outcome_in(&self, session: SessionId, source: FileSource) -> Option<&MutationOutcome> {
        self.browser_in(session, source)?.last_outcome()
    }

    /// Which source a narrow window is showing.
    #[must_use]
    pub fn shown(&self, session: SessionId) -> Option<FileSource> {
        let files = self.sessions.get(&session)?;
        files.remote_browser.is_some().then_some(files.shown)
    }

    /// Shows one source in a narrow window, without changing the other source's path.
    pub fn show(&mut self, session: SessionId, source: FileSource) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if files.remote_browser.is_none() {
            return false;
        }
        files.shown = source;
        true
    }

    /// The entry the keyboard is acting on, when one is chosen.
    #[must_use]
    pub fn selected(&self, session: SessionId, source: FileSource) -> Option<&str> {
        let files = self.sessions.get(&session)?;
        match source {
            FileSource::Local => files.local_selected.as_deref(),
            FileSource::Remote => files.remote_selected.as_deref(),
        }
    }

    /// Remembers which entry the keyboard acts on. `None` clears the choice.
    pub fn select(&mut self, session: SessionId, source: FileSource, name: Option<String>) {
        let Some(files) = self.sessions.get_mut(&session) else {
            return;
        };
        match source {
            FileSource::Local => files.local_selected = name,
            FileSource::Remote => files.remote_selected = name,
        }
    }

    /// How one column orders its listing (R14).
    ///
    /// Each column keeps its own order, per session, so sorting the server's files
    /// leaves this computer's column alone and a layout switch changes nothing.
    #[must_use]
    pub fn sort_of(&self, session: SessionId, source: FileSource) -> Option<FileSort> {
        let files = self.sessions.get(&session)?;
        files.remote_browser.as_ref()?;
        Some(match source {
            FileSource::Local => files.local_sort,
            FileSource::Remote => files.remote_sort,
        })
    }

    /// Orders one column by a key, the way a press on that column's header asks for.
    ///
    /// Returns whether the order changed, so the window redraws only when it did.
    pub fn sort_by(&mut self, session: SessionId, source: FileSource, key: SortKey) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if files.remote_browser.is_none() {
            return false;
        }
        let slot = match source {
            FileSource::Local => &mut files.local_sort,
            FileSource::Remote => &mut files.remote_sort,
        };
        let next = slot.pressed(key);
        if *slot == next {
            return false;
        }
        *slot = next;
        true
    }

    /// The text one column keeps only matching names of (R14).
    #[must_use]
    pub fn filter_of(&self, session: SessionId, source: FileSource) -> Option<&str> {
        let files = self.sessions.get(&session)?;
        files.remote_browser.as_ref()?;
        Some(match source {
            FileSource::Local => &files.local_filter,
            FileSource::Remote => &files.remote_filter,
        })
    }

    /// Filters one column by a name fragment, ignoring case.
    ///
    /// Returns whether the text changed. The listing itself is not re-read: a filter is
    /// a way of reading what the server already sent, so typing into it costs no round
    /// trip.
    pub fn set_filter(
        &mut self,
        session: SessionId,
        source: FileSource,
        text: impl Into<String>,
    ) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if files.remote_browser.is_none() {
            return false;
        }
        let text = text.into();
        let slot = match source {
            FileSource::Local => &mut files.local_filter,
            FileSource::Remote => &mut files.remote_filter,
        };
        if *slot == text {
            return false;
        }
        *slot = text;
        true
    }

    /// Whether one column includes dotfiles (entries whose name starts with `.`).
    #[must_use]
    pub fn show_hidden(&self, session: SessionId, source: FileSource) -> bool {
        let Some(files) = self.sessions.get(&session) else {
            return true;
        };
        match source {
            FileSource::Local => files.local_show_hidden,
            FileSource::Remote => files.remote_show_hidden,
        }
    }

    /// Toggles whether one column shows dotfiles. Returns the new visibility state.
    pub fn toggle_show_hidden(&mut self, session: SessionId, source: FileSource) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return true;
        };
        let slot = match source {
            FileSource::Local => &mut files.local_show_hidden,
            FileSource::Remote => &mut files.remote_show_hidden,
        };
        *slot = !*slot;
        *slot
    }

    /// The rows one column shows: its listing, filtered by the column's own text and
    /// ordered by the column's own sort (R14).
    ///
    /// Nothing is invented here: every row is an entry the server or this computer
    /// really returned, kept in the order the column asks for.
    #[must_use]
    pub fn rows(&self, session: SessionId, source: FileSource) -> Vec<DirEntry> {
        let Some(browser) = self.browser_in(session, source) else {
            return Vec::new();
        };
        let Some(listing) = browser.state().listing() else {
            return Vec::new();
        };
        let needle = self
            .filter_of(session, source)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let show_hidden = self.show_hidden(session, source);
        let sort = self.sort_of(session, source).unwrap_or_else(FileSort::name);
        let mut rows: Vec<DirEntry> = listing
            .entries()
            .iter()
            .filter(|entry| show_hidden || !entry.name().starts_with('.'))
            .filter(|entry| needle.is_empty() || entry.name().to_lowercase().contains(&needle))
            .cloned()
            .collect();
        rows.sort_by(|left, right| compare_entries(left, right, sort));
        rows
    }

    /// How many names one column shows out of how many it holds (R14).
    #[must_use]
    pub fn counts(&self, session: SessionId, source: FileSource) -> Option<(usize, usize)> {
        let total = self
            .browser_in(session, source)?
            .state()
            .listing()?
            .entries()
            .len();
        Some((self.rows(session, source).len(), total))
    }

    /// The entries one column has marked, in the order they were marked (R14).
    #[must_use]
    pub fn marked(&self, session: SessionId, source: FileSource) -> Vec<String> {
        let Some(files) = self.sessions.get(&session) else {
            return Vec::new();
        };
        match source {
            FileSource::Local => files.local_marked.clone(),
            FileSource::Remote => files.remote_marked.clone(),
        }
    }

    /// Whether one entry of a column is marked.
    #[must_use]
    pub fn is_marked(&self, session: SessionId, source: FileSource, name: &str) -> bool {
        let Some(files) = self.sessions.get(&session) else {
            return false;
        };
        match source {
            FileSource::Local => files.local_marked.iter().any(|marked| marked == name),
            FileSource::Remote => files.remote_marked.iter().any(|marked| marked == name),
        }
    }

    /// Adds one entry to a column's marks, or takes it out. Returns whether it is
    /// marked now.
    ///
    /// Only an entry the listing really holds can be marked: a mark on a name that is
    /// not there would be a batch action about a file nothing can reach.
    pub fn toggle_mark(&mut self, session: SessionId, source: FileSource, name: &str) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        let present = side_mut(files, source).is_some_and(|browser| browser.entry(name).is_some());
        if !present {
            return false;
        }
        let marks = match source {
            FileSource::Local => &mut files.local_marked,
            FileSource::Remote => &mut files.remote_marked,
        };
        if let Some(at) = marks.iter().position(|marked| marked == name) {
            marks.remove(at);
            false
        } else {
            marks.push(name.to_owned());
            true
        }
    }

    /// Marks the rows from the one the keyboard is on to the clicked one, both
    /// included (R14).
    ///
    /// The range is the rows in the order the column shows them, so Shift+click marks
    /// what the reader sees between the two rows and not what the server happened to
    /// send between them.
    pub fn mark_range(&mut self, session: SessionId, source: FileSource, from: &str, to: &str) {
        let rows = self.rows(session, source);
        let Some(start) = rows.iter().position(|row| row.name() == from) else {
            return;
        };
        let Some(end) = rows.iter().position(|row| row.name() == to) else {
            return;
        };
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let names: Vec<String> = rows[start..=end]
            .iter()
            .map(|row| row.name().to_owned())
            .collect();
        self.set_marked(session, source, names);
    }

    /// Replaces one column's marks.
    pub fn set_marked(&mut self, session: SessionId, source: FileSource, names: Vec<String>) {
        let Some(files) = self.sessions.get_mut(&session) else {
            return;
        };
        let marks = match source {
            FileSource::Local => &mut files.local_marked,
            FileSource::Remote => &mut files.remote_marked,
        };
        *marks = names;
    }

    /// Drops one column's marks. Returns whether it had any.
    pub fn clear_marked(&mut self, session: SessionId, source: FileSource) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        let marks = match source {
            FileSource::Local => &mut files.local_marked,
            FileSource::Remote => &mut files.remote_marked,
        };
        let had = !marks.is_empty();
        marks.clear();
        had
    }

    /// Marks every row one column shows, or drops the marks (R14).
    pub fn mark_all(&mut self, session: SessionId, source: FileSource, on: bool) -> bool {
        if !on {
            return self.clear_marked(session, source);
        }
        let names: Vec<String> = self
            .rows(session, source)
            .iter()
            .map(|row| row.name().to_owned())
            .collect();
        if names.is_empty() || names == self.marked(session, source) {
            return false;
        }
        self.set_marked(session, source, names);
        true
    }

    /// Asks one source to delete several entries, one request at a time (R14).
    ///
    /// Every name is checked against the listing on screen before anything is sent, so
    /// a batch cannot name an entry that is not there. The first request leaves now and
    /// the rest follow as each answer lands, because a browser holds one request at a
    /// time and a second one would drop the first one's report.
    ///
    /// # Errors
    ///
    /// Returns [`NameProblem`] when there is no listing, when the batch is empty, or
    /// when one of the names is no longer in the directory on screen.
    pub fn delete_many(
        &mut self,
        session: SessionId,
        source: FileSource,
        names: &[String],
    ) -> Result<usize, NameProblem> {
        let Some(files) = self.sessions.get_mut(&session) else {
            return Err(NameProblem::NoDirectory);
        };
        let Some(browser) = side_mut(files, source) else {
            return Err(NameProblem::NoDirectory);
        };
        if names.is_empty() {
            return Err(NameProblem::Missing);
        }
        for name in names {
            if browser.entry(name).is_none() {
                return Err(NameProblem::Missing);
            }
        }
        // A second batch replaces the first one's queue: the confirmation the user just
        // gave is about the rows they see now.
        let queue = match source {
            FileSource::Local => &mut files.local_pending,
            FileSource::Remote => &mut files.remote_pending,
        };
        queue.clear();
        queue.extend(names.iter().cloned());
        advance_deletions(files, source);
        Ok(names.len())
    }

    /// The local directory on screen, as a path of this computer.
    ///
    /// `None` when that directory has not been listed, or when the path is the
    /// Windows drive list, which is not a folder a file can be written into.
    #[must_use]
    pub fn local_os_directory(&self, session: SessionId) -> Option<PathBuf> {
        let path = self
            .browser_in(session, FileSource::Local)?
            .current_directory()?;
        local_protocol_to_os(path)
    }

    /// Opens both sources of a session.
    ///
    /// The remote side asks the connection that is already authenticated for its
    /// login directory. The local side asks this computer. Neither request invents
    /// a listing, and opening does not ask for credentials again.
    pub fn open(&mut self, session: SessionId) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if files.remote_browser.is_none() {
            let mut remote = FileBrowser::new(session, files.host.clone(), files.target.clone());
            // A shell that already said where it is is the directory the user is
            // working in, so the first listing is that one instead of the login
            // directory (R15). A shell that said nothing keeps the login directory,
            // which is what the server itself reports.
            let reported = files.reported.clone().filter(|_| files.follow);
            let request = match &reported {
                Some(path) => {
                    // The note says what the pending listing is: until the server
                    // answers, the column is following that reported directory, and
                    // saying "no directory reported yet" would contradict the request
                    // that was just sent (R15).
                    files.follow_target = Some(path.clone());
                    files.note = FollowNote::Following { path: path.clone() };
                    remote.begin_list(path.clone())
                }
                None => remote.begin_home(),
            };
            files.remote_browser = Some(remote);
            let _ = deliver(files, FileSource::Remote, request);
            let mut local = FileBrowser::new(session, machine_name(), "local");
            let request = local.begin_home();
            files.local_browser = Some(local);
            let _ = deliver(files, FileSource::Local, request);
            files.shown = FileSource::Remote;
        }
        self.is_open(session)
    }

    /// Closes both sources, keeping the connection for a later open.
    pub fn close(&mut self, session: SessionId) {
        if let Some(files) = self.sessions.get_mut(&session) {
            files.remote_browser = None;
            files.local_browser = None;
            files.local_selected = None;
            files.remote_selected = None;
            // The entries the marks and the queue name are gone with the listing: a
            // reopened view cannot act on a row it has not read yet (R14).
            files.local_marked.clear();
            files.remote_marked.clear();
            files.local_pending.clear();
            files.remote_pending.clear();
        }
    }

    /// Forgets a session entirely, which is what a closed connection does.
    ///
    /// The connection ends with the session, so both listings end with it: a path
    /// that nothing backs would keep showing files the user cannot reach.
    pub fn detach(&mut self, session: SessionId) -> bool {
        let had_view = self.is_open(session);
        self.sessions.remove(&session);
        had_view
    }

    /// Asks for the parent of the remote directory shown.
    pub fn up(&mut self, session: SessionId) -> bool {
        self.up_in(session, FileSource::Remote)
    }

    /// Asks for the parent of one source.
    ///
    /// Asking by hand is what turns following off for the remote side: the user chose
    /// this directory, and the next `cd` in the shell must not take it away (R15).
    pub fn up_in(&mut self, session: SessionId, source: FileSource) -> bool {
        // A step the browser cannot take - the column is already at the root, and there
        // is no parent to ask for - is not the user choosing a directory, so following
        // stays on. Turning it off there would leave the switch saying the user took
        // over while nothing moved (R15).
        let moved = self.ask_in(session, source, |browser| browser.up());
        if moved {
            self.stop_following(session, source);
        }
        moved
    }

    /// Asks again for the remote directory shown.
    pub fn refresh(&mut self, session: SessionId) -> bool {
        self.refresh_in(session, FileSource::Remote)
    }

    /// Asks again for one source. The next listing, not the previous one, is truth.
    pub fn refresh_in(&mut self, session: SessionId, source: FileSource) -> bool {
        self.ask_in(session, source, |browser| Some(browser.refresh()))
    }

    /// Whether the remote column of this session follows its own shell (R15).
    #[must_use]
    pub fn follows(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|files| files.follow)
    }

    /// What the follow switch of this session is doing, when it has a view at all.
    #[must_use]
    pub fn follow_note(&self, session: SessionId) -> Option<FollowNote> {
        let files = self.sessions.get(&session)?;
        // A session with no view has nothing to say about following: no switch is
        // drawn for it.
        files.remote_browser.as_ref()?;
        Some(files.note.clone())
    }

    /// Turns following on or off for one session.
    ///
    /// Turning it on returns to the directory the shell last reported, which is the
    /// reason a switch exists: the user browsed away by hand and now wants the
    /// terminal's directory back. Returning `true` means a listing was asked for.
    pub fn set_follow(&mut self, session: SessionId, on: bool) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if !on {
            files.follow = false;
            files.follow_target = None;
            files.note = FollowNote::Manual;
            return false;
        }
        files.follow = true;
        match files.reported.clone() {
            Some(path) => follow_to(files, &path),
            None => {
                files.note = FollowNote::NoReport;
                false
            }
        }
    }

    /// Records a working directory the shell of one session stated with OSC 7 (R15).
    ///
    /// `active` says whether this is the pane the user is working in. A split tab or
    /// a background tab can hold a terminal of another session, and only the active
    /// pane of this session may move this session's listing; the report is still
    /// remembered, so turning following back on goes to where the shell now is.
    ///
    /// Returns whether a listing was asked for.
    pub fn note_terminal_directory(
        &mut self,
        session: SessionId,
        path: &str,
        active: bool,
    ) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        files.reported = Some(path.to_owned());
        if !files.follow {
            return false;
        }
        if files.note
            == (FollowNote::Unreadable {
                path: path.to_owned(),
            })
        {
            // The same directory the server refused once is not asked for again on
            // every prompt: the note stays until the shell moves somewhere else.
            return false;
        }
        if files.remote_browser.is_none() {
            // Nothing is open, so there is no listing to move; the report is kept for
            // the open that follows.
            files.note = FollowNote::Following {
                path: path.to_owned(),
            };
            return false;
        }
        if !active {
            // Another pane of the tab is the one being typed into, so this session's
            // listing is left where it is. The report is remembered and followed once
            // this session is the active one again, so the switch cannot say "following"
            // while the listing shows a directory the shell already left (R15).
            files.deferred_follow = true;
            return false;
        }
        follow_to(files, path)
    }

    /// Follows a directory report that arrived while its session was not the active
    /// pane (R15).
    ///
    /// Called with the session the user is working in, so a report of a background tab
    /// moves nothing: only coming back to that session does.
    pub fn catch_up_follow(&mut self, active: Option<SessionId>) -> bool {
        let Some(session) = active else {
            return false;
        };
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if !files.deferred_follow {
            return false;
        }
        if files.remote_browser.is_none() || !files.follow {
            files.deferred_follow = false;
            return false;
        }
        let Some(path) = files.reported.clone() else {
            files.deferred_follow = false;
            return false;
        };
        files.deferred_follow = false;
        follow_to(files, &path)
    }

    /// Turns following off because the user browsed by hand (R15).
    fn stop_following(&mut self, session: SessionId, source: FileSource) {
        if source != FileSource::Remote {
            return;
        }
        let Some(files) = self.sessions.get_mut(&session) else {
            return;
        };
        if !files.follow {
            return;
        }
        files.follow = false;
        files.follow_target = None;
        files.note = FollowNote::Manual;
    }

    /// Opens one remote entry.
    pub fn enter(&mut self, session: SessionId, name: &str) -> bool {
        self.enter_in(session, FileSource::Remote, name)
    }

    /// Opens one entry of one source.
    ///
    /// Opening a row is browsing by hand, so the remote side stops following (R15).
    pub fn enter_in(&mut self, session: SessionId, source: FileSource, name: &str) -> bool {
        // Only a row this source really opened is browsing by hand: a click on a file,
        // or on a folder that is no longer there, must not stop following either.
        let opened = self.ask_in(session, source, |browser| browser.open(name));
        if opened {
            self.stop_following(session, source);
        }
        opened
    }

    /// Asks one source to list an absolute path it already speaks.
    ///
    /// Remote paths are POSIX. Local paths are the protocol form [`local_protocol_to_os`]
    /// reverses, so a breadcrumb cannot point the remote browser at a local disk.
    pub fn list_at(&mut self, session: SessionId, source: FileSource, path: &str) -> bool {
        self.stop_following(session, source);
        self.ask_in(session, source, |browser| Some(browser.begin_list(path)))
    }

    /// Asks for a new remote directory called name.
    pub fn create_directory(&mut self, session: SessionId, name: &str) -> Result<(), NameProblem> {
        self.create_directory_in(session, FileSource::Remote, name)
    }

    /// Asks one source for a new directory. A refused name sends nothing.
    pub fn create_directory_in(
        &mut self,
        session: SessionId,
        source: FileSource,
        name: &str,
    ) -> Result<(), NameProblem> {
        self.mutate_in(session, source, |browser| browser.create_directory(name))
    }

    /// Asks to rename one remote entry.
    pub fn rename(&mut self, session: SessionId, from: &str, to: &str) -> Result<(), NameProblem> {
        self.rename_in(session, FileSource::Remote, from, to)
    }

    /// Asks one source to rename one entry.
    pub fn rename_in(
        &mut self,
        session: SessionId,
        source: FileSource,
        from: &str,
        to: &str,
    ) -> Result<(), NameProblem> {
        self.mutate_in(session, source, |browser| browser.rename(from, to))
    }

    /// Asks to delete one remote entry.
    ///
    /// The window always goes through delete_many so one deletion and many share the
    /// same queue; this is the shorthand the module's own tests use.
    #[cfg(test)]
    pub fn delete(&mut self, session: SessionId, name: &str) -> Result<(), NameProblem> {
        self.delete_in(session, FileSource::Remote, name)
    }

    /// Asks one source to delete one entry.
    #[cfg(test)]
    pub fn delete_in(
        &mut self,
        session: SessionId,
        source: FileSource,
        name: &str,
    ) -> Result<(), NameProblem> {
        // One entry is the smallest batch there is, so both go through the same queue
        // and the same "one request at a time" rule (R14).
        self.delete_many(session, source, &[name.to_owned()])
            .map(|_count| ())
    }

    /// Runs one change and delivers it to that source only.
    fn mutate_in(
        &mut self,
        session: SessionId,
        source: FileSource,
        ask: impl FnOnce(&mut FileBrowser) -> Result<BrowseRequest, NameProblem>,
    ) -> Result<(), NameProblem> {
        let Some(files) = self.sessions.get_mut(&session) else {
            return Err(NameProblem::NoDirectory);
        };
        let request = {
            let Some(browser) = side_mut(files, source) else {
                return Err(NameProblem::NoDirectory);
            };
            ask(browser)?
        };
        deliver(files, source, request);
        Ok(())
    }

    /// Applies every answer that arrived, returning whether anything changed.
    ///
    /// Each source is applied on its own. A change is followed by a fresh listing of
    /// that same source, so a remote failure cannot replace the local directory and a
    /// local change is not treated as something the server confirmed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        for files in self.sessions.values_mut() {
            if pump_side(files, FileSource::Remote) {
                changed = true;
            }
            if pump_side(files, FileSource::Local) {
                changed = true;
            }
            // A batch of deletions sends its next request only once the one before it
            // and its listing have been answered (R14).
            if advance_deletions(files, FileSource::Remote) {
                changed = true;
            }
            if advance_deletions(files, FileSource::Local) {
                changed = true;
            }
            if settle_follow(files) {
                changed = true;
            }
        }
        changed
    }

    /// Runs one request of one browser and delivers it to that source.
    fn ask_in(
        &mut self,
        session: SessionId,
        source: FileSource,
        ask: impl FnOnce(&mut FileBrowser) -> Option<BrowseRequest>,
    ) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        let request = {
            let Some(browser) = side_mut(files, source) else {
                return false;
            };
            ask(browser)
        };
        let Some(request) = request else {
            return false;
        };
        deliver(files, source, request)
    }
}

/// Orders two rows of one column (R14).
///
/// Folders come first whichever way the column is ordered: the folder a reader is about
/// to open is not ranked against the files beside it. Inside one kind the column's own
/// key decides, and a name breaks the remaining ties so the order cannot wobble between
/// two listings of the same directory.
fn compare_entries(left: &DirEntry, right: &DirEntry, sort: FileSort) -> Ordering {
    let folders = right.is_directory().cmp(&left.is_directory());
    if folders != Ordering::Equal {
        return folders;
    }
    let by = match sort.key {
        SortKey::Name => {
            let order = left.name().to_lowercase().cmp(&right.name().to_lowercase());
            if sort.descending {
                order.reverse()
            } else {
                order
            }
        }
        // A size the server did not report is not zero bytes: such an entry stays at
        // the end of the column in both directions instead of pretending to be the
        // smallest or the largest (R16).
        SortKey::Size => match (left.size(), right.size()) {
            (Some(one), Some(other)) => {
                if sort.descending {
                    other.cmp(&one)
                } else {
                    one.cmp(&other)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        // As with a size, an entry without a time stays at the end either way.
        SortKey::Modified => match (left.modified(), right.modified()) {
            (Some(one), Some(other)) => {
                if sort.descending {
                    other.cmp(&one)
                } else {
                    one.cmp(&other)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    by.then_with(|| left.name().to_lowercase().cmp(&right.name().to_lowercase()))
}

/// Sends the next deletion of one column's batch (R14).
///
/// One request at a time: a browser holds a single outstanding request, and a second
/// one would drop the report of the first. A name that is no longer in the directory is
/// skipped rather than reported, because the listing already shows what is left.
fn advance_deletions(files: &mut SessionFiles, source: FileSource) -> bool {
    let idle = side_mut(files, source)
        .is_some_and(|browser| !browser.is_waiting() && !browser.is_changing());
    if !idle {
        return false;
    }
    let next = {
        let queue = match source {
            FileSource::Local => &mut files.local_pending,
            FileSource::Remote => &mut files.remote_pending,
        };
        queue.pop_front()
    };
    let Some(name) = next else {
        return false;
    };
    let request = side_mut(files, source).and_then(|browser| browser.delete(&name).ok());
    match request {
        Some(request) => deliver(files, source, request),
        None => false,
    }
}

/// The browser of one source.
fn side_mut(files: &mut SessionFiles, source: FileSource) -> Option<&mut FileBrowser> {
    match source {
        FileSource::Local => files.local_browser.as_mut(),
        FileSource::Remote => files.remote_browser.as_mut(),
    }
}

/// Applies one source's answers and, after a change, asks that source to list again.
fn pump_side(files: &mut SessionFiles, source: FileSource) -> bool {
    let responses = match source {
        FileSource::Local => files.local.drain(),
        FileSource::Remote => files.remote.drain(),
    };
    let mut changed = false;
    for response in responses {
        let is_change = response.outcome().is_some();
        let applied = side_mut(files, source).is_some_and(|browser| browser.apply(response));
        if !applied {
            continue;
        }
        changed = true;
        forget_missing_selection(files, source);
        forget_missing_marks(files, source);
        if source == FileSource::Remote {
            // The last directory the server itself confirmed, which is where a
            // reported path that cannot be read returns to (R15).
            if let Some(path) = side_mut(files, source)
                .and_then(|browser| browser.current_directory())
                .map(str::to_owned)
            {
                files.last_good = Some(path);
            }
        }
        if is_change {
            let Some(browser) = side_mut(files, source) else {
                continue;
            };
            let refreshed = browser.refresh();
            deliver(files, source, refreshed);
        }
    }
    changed
}

/// Drops the marks of entries the latest listing no longer contains (R14).
///
/// A marked row that a Refresh no longer returns cannot be part of a batch: the mark
/// would name a file nothing can reach, and the batch would ask a server to delete it.
fn forget_missing_marks(files: &mut SessionFiles, source: FileSource) {
    let kept: Vec<String> = {
        let marks = match source {
            FileSource::Local => files.local_marked.clone(),
            FileSource::Remote => files.remote_marked.clone(),
        };
        let browser = match source {
            FileSource::Local => files.local_browser.as_ref(),
            FileSource::Remote => files.remote_browser.as_ref(),
        };
        match browser {
            Some(browser) => marks
                .into_iter()
                .filter(|name| browser.entry(name).is_some())
                .collect(),
            // No listing at all: nothing a mark could still name is on screen.
            None => Vec::new(),
        }
    };
    match source {
        FileSource::Local => files.local_marked = kept,
        FileSource::Remote => files.remote_marked = kept,
    }
}

/// Drops a selection the latest listing no longer contains.
fn forget_missing_selection(files: &mut SessionFiles, source: FileSource) {
    let selected = match source {
        FileSource::Local => files.local_selected.clone(),
        FileSource::Remote => files.remote_selected.clone(),
    };
    let still = selected.as_deref().is_some_and(|name| {
        side_mut(files, source).is_some_and(|browser| browser.entry(name).is_some())
    });
    if !still {
        match source {
            FileSource::Local => files.local_selected = None,
            FileSource::Remote => files.remote_selected = None,
        }
    }
}

/// Delivers one request to one source.
///
/// A request that never left the process is a failure of that source only. The
/// remote wording stays the one the connection already uses, so a lost SSH session
/// is not described as a problem with this computer.
fn deliver(files: &mut SessionFiles, source: FileSource, request: BrowseRequest) -> bool {
    let sent = match source {
        FileSource::Local => files.local.send(request.clone()),
        FileSource::Remote => files.remote.send(request.clone()),
    };
    if sent {
        return true;
    }
    if let Some(browser) = side_mut(files, source) {
        let reason = match source {
            FileSource::Remote => "the SSH connection is no longer open".to_owned(),
            FileSource::Local => "this computer could not accept the request".to_owned(),
        };
        browser.apply(BrowseResponse::Failed {
            request: request.request(),
            failure: BrowseFailure::ConnectionLost { reason },
        });
    }
    false
}

/// Asks the remote side of one session for the directory the shell reported (R15).
///
/// A view that is already showing that directory is left alone, so a prompt that
/// reports the same directory does not replace the listing on screen.
fn follow_to(files: &mut SessionFiles, path: &str) -> bool {
    let Some(browser) = files.remote_browser.as_mut() else {
        return false;
    };
    if browser.path() == Some(path) && !browser.is_waiting() && !browser.is_changing() {
        files.follow_target = None;
        files.note = FollowNote::Following {
            path: path.to_owned(),
        };
        return false;
    }
    // Only the newest request counts, and the browser drops an answer for any other
    // one, so a listing that arrives late cannot put an older directory back (R15).
    files.follow_target = Some(path.to_owned());
    files.note = FollowNote::Following {
        path: path.to_owned(),
    };
    let request = browser.begin_list(path);
    deliver(files, FileSource::Remote, request)
}

/// Finishes a follow request once its answer arrived.
///
/// A directory the server refused is stated, and the column returns to the last
/// directory it really listed: following is not allowed to leave the remote side
/// pointing at a path nothing can read, and it does not silently stop either, because
/// the note and the switch are how the user takes over (R15).
fn settle_follow(files: &mut SessionFiles) -> bool {
    let Some(target) = files.follow_target.clone() else {
        return false;
    };
    let Some(browser) = files.remote_browser.as_ref() else {
        files.follow_target = None;
        return false;
    };
    if browser.is_waiting() || browser.is_changing() {
        return false;
    }
    if browser.path() != Some(target.as_str()) {
        // Another request went out in the meantime; its answer is the truth.
        files.follow_target = None;
        return false;
    }
    match browser.state() {
        BrowseState::Ready(_) => {
            files.follow_target = None;
            files.last_good = Some(target.clone());
            files.note = FollowNote::Following { path: target };
            true
        }
        BrowseState::Failed(_) => {
            files.follow_target = None;
            files.note = FollowNote::Unreadable {
                path: target.clone(),
            };
            let Some(good) = files.last_good.clone().filter(|good| *good != target) else {
                // Nothing was ever read here, so there is nowhere to return to and the
                // failure card stays with its own Refresh.
                return true;
            };
            let Some(browser) = files.remote_browser.as_mut() else {
                return true;
            };
            let request = browser.begin_list(good);
            deliver(files, FileSource::Remote, request);
            true
        }
        BrowseState::Loading => false,
    }
}

/// The name of this computer, so the local column can be told from the remote host.
fn machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

/// A path this computer can open, or `None` for the Windows list of drives.
///
/// FileBrowser stores POSIX-shaped paths. On Windows a drive root is `/C:/Users`,
/// which is not a path `std::fs` can open until the drive letter is put back.
#[must_use]
pub fn local_protocol_to_os(path: &str) -> Option<PathBuf> {
    let path = normalize_path(path);
    if cfg!(windows) {
        if path == "/" {
            return None;
        }
        let rest = path.trim_start_matches('/');
        if let Some((drive, tail)) = rest.split_once(':')
            && drive.len() == 1
            && drive
                .chars()
                .next()
                .is_some_and(|letter| letter.is_ascii_alphabetic())
        {
            let mut os = format!("{}:\\", drive.to_ascii_uppercase());
            let tail = tail.trim_start_matches('/');
            if !tail.is_empty() {
                os.push_str(&tail.replace('/', "\\"));
            }
            return Some(PathBuf::from(os));
        }
        return Some(PathBuf::from(rest.replace('/', "\\")));
    }
    Some(PathBuf::from(path))
}

/// The protocol path of a real local path, so both browsers share one path language.
#[must_use]
pub fn local_os_to_protocol(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix("//?/"))
        .unwrap_or(text.as_ref());
    let slash = text.replace('\\', "/");
    if slash.starts_with('/') {
        normalize_path(&slash)
    } else {
        normalize_path(&format!("/{slash}"))
    }
}

/// What the local column prints for a path. The Windows drive list has no folder.
#[must_use]
pub fn display_path(source: FileSource, path: &str) -> String {
    if source == FileSource::Local {
        return local_protocol_to_os(path)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| path.to_owned());
    }
    path.to_owned()
}

/// One step of a path, from the root out to the directory on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathCrumb {
    /// The single name the user clicks.
    pub label: String,
    /// The absolute path that click lists.
    pub path: String,
}

/// The breadcrumbs of one protocol path, root first.
#[must_use]
pub fn path_crumbs(path: &str) -> Vec<PathCrumb> {
    let path = normalize_path(path);
    let mut crumbs = vec![PathCrumb {
        label: "/".to_owned(),
        path: "/".to_owned(),
    }];
    if path == "/" {
        return crumbs;
    }
    let mut built = String::new();
    for part in path.trim_start_matches('/').split('/') {
        if part.is_empty() {
            continue;
        }
        built.push('/');
        built.push_str(part);
        crumbs.push(PathCrumb {
            label: part.to_owned(),
            path: built.clone(),
        });
    }
    crumbs
}

/// Answers browse requests against this computer.
///
/// The requests and the answers are the ones the remote connection already speaks,
/// so a button, a key and a drag cannot grow a second set of file operations.
#[derive(Default)]
struct LocalFs {
    pending: Vec<BrowseResponse>,
}

impl LocalFs {
    fn new() -> Self {
        Self::default()
    }
}

impl BrowseChannel for LocalFs {
    fn send(&mut self, request: BrowseRequest) -> bool {
        self.pending.push(local_answer(&request));
        true
    }

    fn drain(&mut self) -> Vec<BrowseResponse> {
        std::mem::take(&mut self.pending)
    }
}

fn local_answer(request: &BrowseRequest) -> BrowseResponse {
    let number = request.request();
    match request {
        BrowseRequest::Home { .. } => match local_home() {
            Ok(path) => match list_directory(&path) {
                Ok(entries) => BrowseResponse::Listed {
                    request: number,
                    path: local_os_to_protocol(&path),
                    entries,
                },
                Err(failure) => BrowseResponse::Failed {
                    request: number,
                    failure,
                },
            },
            Err(failure) => BrowseResponse::Failed {
                request: number,
                failure,
            },
        },
        BrowseRequest::List { path, .. } => match list_protocol(path) {
            Ok(entries) => BrowseResponse::Listed {
                request: number,
                path: normalize_path(path),
                entries,
            },
            Err(failure) => BrowseResponse::Failed {
                request: number,
                failure,
            },
        },
        BrowseRequest::CreateDirectory {
            directory, name, ..
        } => {
            let target = join_path(directory, name);
            let action = MutationAction::CreateDirectory {
                path: target.clone(),
            };
            let outcome = match create_local_directory(directory, name) {
                Ok(()) => MutationOutcome::new(
                    action,
                    vec![MutationStep::done(target, Some(FileKind::Directory))],
                ),
                Err(failure) => MutationOutcome::refused(action, failure),
            };
            BrowseResponse::Changed {
                request: number,
                outcome,
            }
        }
        BrowseRequest::Rename { from, to, .. } => {
            let action = MutationAction::Rename {
                from: from.clone(),
                to: to.clone(),
            };
            let outcome = match rename_local(from, to) {
                Ok(()) => MutationOutcome::new(action, vec![MutationStep::done(to.clone(), None)]),
                Err(failure) => MutationOutcome::refused(action, failure),
            };
            BrowseResponse::Changed {
                request: number,
                outcome,
            }
        }
        BrowseRequest::Delete { path, kind, .. } => {
            let action = MutationAction::Delete {
                path: path.clone(),
                kind: *kind,
            };
            let outcome = match delete_local(path, *kind) {
                Ok(()) => MutationOutcome::new(
                    action,
                    vec![MutationStep::done(path.clone(), Some(*kind))],
                ),
                Err(failure) => MutationOutcome::refused(action, failure),
            };
            BrowseResponse::Changed {
                request: number,
                outcome,
            }
        }
    }
}

fn local_home() -> Result<PathBuf, BrowseFailure> {
    let raw = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    };
    if let Some(raw) = raw {
        let path = PathBuf::from(raw);
        if path.is_dir() {
            return Ok(path);
        }
    }
    std::env::current_dir().map_err(|error| io_failure(error, "."))
}

fn list_protocol(path: &str) -> Result<Vec<shellune_core::DirEntry>, BrowseFailure> {
    let protocol = normalize_path(path);
    #[cfg(windows)]
    if protocol == "/" {
        return Ok(windows_drives());
    }
    let Some(os_path) = local_protocol_to_os(&protocol) else {
        return Err(BrowseFailure::NotFound { path: protocol });
    };
    list_directory(&os_path)
}

fn list_directory(path: &Path) -> Result<Vec<shellune_core::DirEntry>, BrowseFailure> {
    let protocol = local_os_to_protocol(path);
    let meta = std::fs::metadata(path).map_err(|error| io_failure(error, &protocol))?;
    if !meta.is_dir() {
        return Err(BrowseFailure::NotADirectory { path: protocol });
    }
    let read = std::fs::read_dir(path).map_err(|error| io_failure(error, &protocol))?;
    let mut entries = Vec::new();
    for item in read {
        let Ok(item) = item else {
            continue;
        };
        let name = item.file_name();
        let name = name.to_string_lossy();
        let kind = entry_kind(item.path().as_path());
        let meta = item.metadata().ok();
        let size = (kind == FileKind::File)
            .then(|| meta.as_ref().map(std::fs::Metadata::len))
            .flatten();
        let modified = meta
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_secs());
        if let Some(entry) = shellune_core::DirEntry::new(name.as_ref(), kind, size) {
            entries.push(entry.with_modified(modified));
        }
    }
    Ok(entries)
}

fn entry_kind(path: &Path) -> FileKind {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return FileKind::Other;
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Directory
    } else if file_type.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn create_local_directory(directory: &str, name: &str) -> Result<(), BrowseFailure> {
    // The name is judged before the parent is joined. On Windows `Path::join`
    // treats `..\outside`, `D:\abs` and `\Windows\...` as paths outside the parent,
    // and a later create_dir would write there.
    if !is_single_local_name(name) {
        return Err(name_leaves_directory());
    }
    let Some(parent) = local_protocol_to_os(directory) else {
        return Err(BrowseFailure::Refused {
            reason: "a drive list is not a folder".to_owned(),
        });
    };
    let path = local_child_path(&parent, name)?;
    std::fs::create_dir(&path).map_err(|error| io_failure(error, &local_os_to_protocol(&path)))
}

fn rename_local(from: &str, to: &str) -> Result<(), BrowseFailure> {
    let leaf = confined_leaf(to)?;
    if parent_path(from) != parent_path(to) {
        return Err(name_leaves_directory());
    }
    let Some(from_path) = local_protocol_to_os(from) else {
        return Err(BrowseFailure::NotFound {
            path: from.to_owned(),
        });
    };
    let Some(parent) = from_path.parent() else {
        return Err(name_leaves_directory());
    };
    let to_path = local_child_path(parent, leaf)?;
    std::fs::rename(&from_path, &to_path).map_err(|error| io_failure(error, from))
}

/// A local entry name that stays one component of the directory it was typed in.
///
/// Slashes, backslashes and drive prefixes are rejected here, not in the shared
/// POSIX name check: a remote file may contain a backslash, and `Path::join` only
/// escapes on this computer.
fn is_single_local_name(name: &str) -> bool {
    if name.is_empty() || name.contains(['/', '\\', '\0']) {
        return false;
    }
    let mut bytes = name.chars();
    if bytes
        .next()
        .is_some_and(|letter| letter.is_ascii_alphabetic())
        && bytes.next() == Some(':')
    {
        return false;
    }
    let mut components = Path::new(name).components();
    match components.next() {
        Some(std::path::Component::Normal(part)) => {
            components.next().is_none() && part == std::ffi::OsStr::new(name)
        }
        _ => false,
    }
}

/// The last segment of a protocol path, when that segment cannot leave its directory.
fn confined_leaf(path: &str) -> Result<&str, BrowseFailure> {
    let trimmed = path.trim_end_matches('/');
    let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if is_single_local_name(leaf) {
        Ok(leaf)
    } else {
        Err(name_leaves_directory())
    }
}

/// `parent` plus one confined name. The name is checked again so a join cannot run first.
fn local_child_path(parent: &Path, name: &str) -> Result<PathBuf, BrowseFailure> {
    if !is_single_local_name(name) {
        return Err(name_leaves_directory());
    }
    let mut path = parent.to_path_buf();
    path.push(name);
    if path.parent() != Some(parent) || path.file_name() != Some(std::ffi::OsStr::new(name)) {
        return Err(name_leaves_directory());
    }
    Ok(path)
}

fn name_leaves_directory() -> BrowseFailure {
    BrowseFailure::Refused {
        reason: "the name leaves this directory".to_owned(),
    }
}

fn delete_local(path: &str, kind: FileKind) -> Result<(), BrowseFailure> {
    let Some(os_path) = local_protocol_to_os(path) else {
        return Err(BrowseFailure::NotFound {
            path: path.to_owned(),
        });
    };
    let removed = match kind {
        FileKind::Directory => std::fs::remove_dir_all(&os_path),
        FileKind::File | FileKind::Symlink | FileKind::Other => std::fs::remove_file(&os_path),
    };
    removed.map_err(|error| io_failure(error, path))
}

#[cfg(windows)]
fn windows_drives() -> Vec<shellune_core::DirEntry> {
    (b'A'..=b'Z')
        .filter_map(|letter| {
            let root = format!("{}:\\", letter as char);
            if std::path::Path::new(&root).is_dir() {
                shellune_core::DirEntry::new(
                    format!("{}:", letter as char),
                    FileKind::Directory,
                    None,
                )
            } else {
                None
            }
        })
        .collect()
}

fn io_failure(error: std::io::Error, path: &str) -> BrowseFailure {
    let path = path.to_owned();
    match error.kind() {
        std::io::ErrorKind::NotFound => BrowseFailure::NotFound { path },
        std::io::ErrorKind::PermissionDenied => BrowseFailure::PermissionDenied { path },
        std::io::ErrorKind::AlreadyExists => BrowseFailure::AlreadyExists { path },
        std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::UnexpectedEof => BrowseFailure::ConnectionLost {
            reason: error.to_string(),
        },
        _ => {
            let raw = error.raw_os_error();
            // ENOTDIR on Unix, ERROR_DIRECTORY on Windows.
            if raw == Some(20) || raw == Some(267) {
                return BrowseFailure::NotADirectory { path };
            }
            let text = error.to_string();
            if text.to_lowercase().contains("not a directory") {
                BrowseFailure::NotADirectory { path }
            } else {
                BrowseFailure::Refused { reason: text }
            }
        }
    }
}
/// One entry a delete question is about (R14).
///
/// The name and the kind travel together from the listing, so the question states what
/// is being deleted and whether it takes anything with it, and the request that follows
/// cannot be about a different entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteTarget {
    /// The entry as the listing named it.
    pub name: String,
    /// What the entry is, as the listing reported it.
    pub kind: FileKind,
}

impl DeleteTarget {
    /// One entry of a listing.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: FileKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }

    /// Whether deleting this entry takes other entries with it.
    #[must_use]
    pub fn is_folder(&self) -> bool {
        self.kind.is_directory()
    }
}

/// The question the file view is asking before anything is sent (R11, R10).
///
/// A change to a server is asked for twice: once in the dialog, where the user sees
/// the exact target, the kind and - for a directory - that its contents go too, and
/// once when the answer arrives. Nothing is sent while this exists, so cancelling is
/// never a request the server has to ignore.
///
/// The two transfer questions are shaped the same way: the upload names the file and
/// the directory it would land in, the download names the entry and the folder it would
/// be written to. Neither starts a task until it is answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesDialog {
    /// A new directory is named but not created.
    NewDirectory {
        /// The session whose directory it would go into.
        session: SessionId,
        /// Which source the directory belongs to.
        source: FileSource,
        /// What the user typed.
        text: String,
        /// Why the last attempt to send it was refused, when it was.
        problem: Option<NameProblem>,
    },
    /// One entry is being renamed.
    Rename {
        /// The session whose directory holds the entry.
        session: SessionId,
        /// Which source holds the entry.
        source: FileSource,
        /// The entry as the listing named it.
        from: String,
        /// The name the user typed.
        text: String,
        /// Why the last attempt to send it was refused, when it was.
        problem: Option<NameProblem>,
    },
    /// One or more entries are being deleted, with everything inside a folder (R14).
    ///
    /// The entries are facts from the listing rather than something the user typed, and
    /// every one of them is named in the question: a batch that says only "4 entries"
    /// would ask the user to confirm a deletion they cannot check.
    Delete {
        /// The session whose directory holds the entries.
        session: SessionId,
        /// Which source holds the entries.
        source: FileSource,
        /// The entries as the listing named them, with what each one is.
        entries: Vec<DeleteTarget>,
        /// Why the deletion could not be sent, when it could not.
        ///
        /// A confirmation that is answered with nothing is the one thing this dialog
        /// must never do: the entry may have vanished since the listing was read, and
        /// that is a reason the user has to see rather than a button that does nothing.
        problem: Option<NameProblem>,
    },
    /// A local file is chosen for upload but nothing was sent yet.
    Upload {
        /// The session whose directory would receive the file.
        session: SessionId,
        /// The source that would receive the file. An upload is always remote.
        source: FileSource,
        /// The remote directory the file would land in.
        ///
        /// It starts at the directory on screen and is editable, because a server
        /// without an SFTP subsystem has no directory on screen and a traditional SCP
        /// copy is still allowed there - the user types the path instead.
        directory: String,
        /// The local path the user typed.
        text: String,
        /// Which protocol would copy it, chosen for this task.
        protocol: TransferProtocol,
        /// Whether a directory source would be copied with its contents.
        recursive: bool,
        /// Why the last attempt could not start, when it could not.
        problem: Option<PathProblem>,
    },
    /// A remote entry is chosen for download but nothing was sent yet.
    Download {
        /// The session the entry belongs to.
        session: SessionId,
        /// The source the entry is copied from. A download is always remote.
        source: FileSource,
        /// The remote path of the entry, editable for the same reason an upload's
        /// directory is.
        remote: String,
        /// The local directory the user typed.
        text: String,
        /// Which protocol would copy it, chosen for this task.
        protocol: TransferProtocol,
        /// Whether a remote directory would be copied with its contents.
        recursive: bool,
        /// Why the last attempt could not start, when it could not.
        problem: Option<PathProblem>,
    },
    /// Jump directly to a typed folder path on one column.
    GoTo {
        /// The session whose column is jumping.
        session: SessionId,
        /// Which source the path belongs to.
        source: FileSource,
        /// The path the user typed.
        text: String,
        /// Why the path could not be used, when it was empty.
        problem: Option<PathProblem>,
    },
}

impl FilesDialog {
    /// The session this question belongs to.
    #[must_use]
    pub fn session(&self) -> SessionId {
        match self {
            Self::NewDirectory { session, .. }
            | Self::Rename { session, .. }
            | Self::Delete { session, .. }
            | Self::Upload { session, .. }
            | Self::Download { session, .. }
            | Self::GoTo { session, .. } => *session,
        }
    }

    /// The source this question would change.
    #[must_use]
    pub fn source(&self) -> FileSource {
        match self {
            Self::NewDirectory { source, .. }
            | Self::Rename { source, .. }
            | Self::Delete { source, .. }
            | Self::Upload { source, .. }
            | Self::Download { source, .. }
            | Self::GoTo { source, .. } => *source,
        }
    }

    /// The text the user is typing, when the question has a field.
    #[cfg(test)]
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::NewDirectory { text, .. }
            | Self::Rename { text, .. }
            | Self::Upload { text, .. }
            | Self::Download { text, .. }
            | Self::GoTo { text, .. } => Some(text),
            Self::Delete { .. } => None,
        }
    }

    /// Replaces the typed text and forgets the previous refusal.
    pub fn set_text(&mut self, typed: String) {
        match self {
            Self::NewDirectory { text, problem, .. } | Self::Rename { text, problem, .. } => {
                *text = typed;
                *problem = None;
            }
            Self::Upload { text, problem, .. }
            | Self::Download { text, problem, .. }
            | Self::GoTo { text, problem, .. } => {
                *text = typed;
                *problem = None;
            }
            Self::Delete { .. } => {}
        }
    }

    /// Replaces the remote path of a transfer question and forgets the last refusal.
    ///
    /// The remote side of an upload is the directory its file would land in; the remote
    /// side of a download is the entry itself. Either way it is the path the user typed,
    /// so a server that offers no directory listing is still usable.
    pub fn set_remote(&mut self, typed: String) {
        match self {
            Self::Upload {
                directory, problem, ..
            } => {
                *directory = typed;
                *problem = None;
            }
            Self::Download {
                remote, problem, ..
            } => {
                *remote = typed;
                *problem = None;
            }
            Self::NewDirectory { .. }
            | Self::Rename { .. }
            | Self::Delete { .. }
            | Self::GoTo { .. } => {}
        }
    }

    /// The remote path of a transfer question, when it has one.
    #[must_use]
    pub fn remote(&self) -> Option<&str> {
        match self {
            Self::Upload { directory, .. } => Some(directory),
            Self::Download { remote, .. } => Some(remote),
            Self::NewDirectory { .. }
            | Self::Rename { .. }
            | Self::Delete { .. }
            | Self::GoTo { .. } => None,
        }
    }

    /// Which protocol this question would copy with, when it is a transfer question.
    #[must_use]
    pub fn protocol(&self) -> Option<TransferProtocol> {
        match self {
            Self::Upload { protocol, .. } | Self::Download { protocol, .. } => Some(*protocol),
            Self::NewDirectory { .. }
            | Self::Rename { .. }
            | Self::Delete { .. }
            | Self::GoTo { .. } => None,
        }
    }

    /// Chooses the protocol for this one transfer.
    ///
    /// The choice starts at the host's default and belongs to this task: it never
    /// changes the host, and a failure of the chosen protocol is reported as that
    /// protocol's failure instead of being retried over the other one.
    pub fn set_protocol(&mut self, chosen: TransferProtocol) {
        match self {
            Self::Upload {
                protocol,
                recursive,
                problem,
                ..
            }
            | Self::Download {
                protocol,
                recursive,
                problem,
                ..
            } => {
                *protocol = chosen;
                if !chosen.copies_directories() {
                    // Only SCP copies a directory, and only when it was asked to.
                    *recursive = false;
                }
                *problem = None;
            }
            Self::NewDirectory { .. }
            | Self::Rename { .. }
            | Self::Delete { .. }
            | Self::GoTo { .. } => {}
        }
    }

    /// Whether this question would copy a directory with its contents.
    #[must_use]
    pub fn recursive(&self) -> bool {
        match self {
            Self::Upload { recursive, .. } | Self::Download { recursive, .. } => *recursive,
            Self::NewDirectory { .. }
            | Self::Rename { .. }
            | Self::Delete { .. }
            | Self::GoTo { .. } => false,
        }
    }

    /// Turns the explicit recursive choice on or off.
    pub fn set_recursive(&mut self, recursive: bool) {
        match self {
            Self::Upload {
                recursive: current,
                protocol,
                problem,
                ..
            }
            | Self::Download {
                recursive: current,
                protocol,
                problem,
                ..
            } => {
                *current = recursive && protocol.copies_directories();
                *problem = None;
            }
            Self::NewDirectory { .. }
            | Self::Rename { .. }
            | Self::Delete { .. }
            | Self::GoTo { .. } => {}
        }
    }

    /// States why the text cannot be sent, keeping the text in front of the user.
    pub fn fail(&mut self, failure: NameProblem) {
        match self {
            Self::NewDirectory { problem, .. }
            | Self::Rename { problem, .. }
            | Self::Delete { problem, .. } => {
                *problem = Some(failure);
            }
            Self::Upload { .. } | Self::Download { .. } | Self::GoTo { .. } => {}
        }
    }

    /// States why a transfer could not start, keeping what the user typed.
    pub fn fail_path(&mut self, failure: PathProblem) {
        match self {
            Self::Upload { problem, .. }
            | Self::Download { problem, .. }
            | Self::GoTo { problem, .. } => {
                *problem = Some(failure);
            }
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => {}
        }
    }

    /// Why the text cannot be sent, when it cannot.
    #[must_use]
    pub fn problem(&self) -> Option<&NameProblem> {
        match self {
            Self::NewDirectory { problem, .. }
            | Self::Rename { problem, .. }
            | Self::Delete { problem, .. } => problem.as_ref(),
            Self::Upload { .. } | Self::Download { .. } | Self::GoTo { .. } => None,
        }
    }

    /// Why a transfer could not start, when it could not.
    #[must_use]
    pub fn path_problem(&self) -> Option<&PathProblem> {
        match self {
            Self::Upload { problem, .. }
            | Self::Download { problem, .. }
            | Self::GoTo { problem, .. } => problem.as_ref(),
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => None,
        }
    }

    /// The entries a delete question is about, and nothing for any other question (R14).
    #[must_use]
    pub fn targets(&self) -> &[DeleteTarget] {
        match self {
            Self::Delete { entries, .. } => entries,
            _ => &[],
        }
    }

    /// How many of those entries are folders, which take their contents with them.
    #[must_use]
    pub fn target_folders(&self) -> usize {
        self.targets()
            .iter()
            .filter(|target| target.is_folder())
            .count()
    }
}

/// What the transfer questions say about a path the window refused, in the current
/// language.
///
/// The domain names the reason; the words come from the string table, because a reason
/// the user cannot read is the same as no reason at all.
#[must_use]
pub fn path_problem_text(problem: &PathProblem, strings: &Strings) -> &'static str {
    match problem {
        PathProblem::Empty => strings.transfers_problem_empty_path,
        PathProblem::NoName => strings.transfers_problem_no_name,
        PathProblem::NeedsRecursion => strings.transfers_problem_needs_recursion,
        PathProblem::DirectoryNeedsScp => strings.transfers_problem_directory_needs_scp,
    }
}

/// What the view says when a transfer cannot be started at all.
#[must_use]
pub fn transfer_problem_text(problem: &TransferProblem, strings: &Strings) -> &'static str {
    match problem {
        TransferProblem::SourceNamesNoFile => strings.transfers_problem_no_source,
        TransferProblem::DirectoryUnknown => strings.transfers_problem_no_directory,
        TransferProblem::CommandRefused => strings.transfers_problem_no_connection,
        TransferProblem::DirectoryNeedsRecursion => strings.transfers_problem_needs_recursion,
        TransferProblem::DirectoryNeedsScp => strings.transfers_problem_directory_needs_scp,
    }
}

/// What the view says about a name it refused, in the current language.
///
/// The domain names the reason; only the words come from here, because a reason the
/// user cannot read is the same as no reason at all.
#[must_use]
pub fn problem_text(problem: &NameProblem, strings: &Strings) -> &'static str {
    match problem {
        NameProblem::NoDirectory => strings.files_problem_no_directory,
        NameProblem::Empty => strings.files_problem_empty,
        NameProblem::Navigation => strings.files_problem_navigation,
        NameProblem::Separator => strings.files_problem_separator,
        NameProblem::Taken => strings.files_problem_taken,
        NameProblem::Missing => strings.files_problem_missing,
        NameProblem::Unchanged => strings.files_problem_unchanged,
    }
}

/// What one source says about a directory it could not read.
///
/// The remote wording is the failure's own, which already names the server. The
/// local wording must not say "server": a folder on this computer is a different
/// object, and the two reasons have to stay distinguishable.
#[must_use]
pub fn browse_failure_text(
    failure: &BrowseFailure,
    source: FileSource,
    strings: &Strings,
) -> String {
    if source == FileSource::Remote {
        return failure.to_string();
    }
    let path = failure.path().unwrap_or("");
    match failure {
        BrowseFailure::Unavailable { reason } => {
            format!("{} {reason}", strings.files_local_unavailable)
        }
        BrowseFailure::NotFound { .. } => format!("{} {path}", strings.files_local_not_found),
        BrowseFailure::NotADirectory { .. } => {
            format!("{} {path}", strings.files_local_not_directory)
        }
        BrowseFailure::PermissionDenied { .. } => {
            format!("{} {path}", strings.files_local_denied)
        }
        BrowseFailure::AlreadyExists { .. } => format!("{} {path}", strings.files_local_exists),
        BrowseFailure::ConnectionLost { reason } => {
            format!("{} {reason}", strings.files_local_lost)
        }
        BrowseFailure::Refused { reason } => format!("{} {reason}", strings.files_local_refused),
    }
}

/// What the view prints for one kind of entry, in the current language.
#[must_use]
pub fn kind_label(kind: FileKind, strings: &Strings) -> &'static str {
    match kind {
        FileKind::Directory => strings.files_kind_directory,
        FileKind::File => strings.files_kind_file,
        FileKind::Symlink => strings.files_kind_link,
        FileKind::Other => strings.files_kind_other,
    }
}

/// How long a pause ends a type-ahead word: the next letter starts a new one.
pub const TYPE_AHEAD_PAUSE: std::time::Duration = std::time::Duration::from_millis(1000);

/// The row a type-ahead word lands on, the way a file manager's listing answers letters.
///
/// `typed` is every letter of the word so far, the newest last. A word of one letter, or
/// of one letter pressed again and again, steps to the next row after `current` that
/// starts with that letter and wraps around, so "d d d" walks through every folder
/// whose name starts with d. A longer word keeps the row it is on while that row still
/// matches, and otherwise finds the first row after it that does. Case is ignored;
/// `None` means no row starts with the word, and the selection stays where it was.
#[must_use]
pub fn type_ahead_target(names: &[String], current: Option<usize>, typed: &str) -> Option<usize> {
    let word = typed.to_lowercase();
    let mut letters = word.chars();
    let first = letters.next()?;
    let repeated = letters.all(|letter| letter == first);
    let (needle, start) = if repeated {
        (first.to_string(), current.map_or(0, |index| index + 1))
    } else {
        (word.clone(), current.unwrap_or(0))
    };
    let len = names.len();
    (0..len)
        .map(|offset| (start + offset) % len)
        .find(|&index| names[index].to_lowercase().starts_with(&needle))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn listing(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_letter_steps_through_the_rows_that_start_with_it_and_wraps() {
        let names = listing(&["bin", "Desktop", "docs", "downloads", "etc"]);
        assert_eq!(type_ahead_target(&names, None, "d"), Some(1), "any case");
        assert_eq!(type_ahead_target(&names, Some(1), "d"), Some(2));
        assert_eq!(
            type_ahead_target(&names, Some(2), "dd"),
            Some(3),
            "pressed again"
        );
        assert_eq!(type_ahead_target(&names, Some(3), "ddd"), Some(1), "wraps");
        assert_eq!(type_ahead_target(&names, Some(3), "z"), None);
        assert_eq!(type_ahead_target(&names, Some(0), ""), None);
        assert_eq!(type_ahead_target(&[], None, "a"), None);
    }

    #[test]
    fn a_word_keeps_the_row_it_still_names_and_moves_on_when_it_does_not() {
        let names = listing(&["bin", "Desktop", "docs", "downloads", "etc"]);
        // "d" landed on Desktop; "do" no longer names it, so the word moves on.
        assert_eq!(type_ahead_target(&names, Some(1), "do"), Some(2));
        assert_eq!(type_ahead_target(&names, Some(2), "dow"), Some(3));
        assert_eq!(
            type_ahead_target(&names, Some(1), "de"),
            Some(1),
            "still named"
        );
        assert_eq!(type_ahead_target(&names, Some(4), "bi"), Some(0), "wraps");
        assert_eq!(type_ahead_target(&names, Some(0), "bx"), None);
    }
    use shellune_core::DirEntry;

    fn stamped(name: &str, kind: FileKind, modified: Option<u64>) -> DirEntry {
        DirEntry::new(name, kind, Some(1))
            .expect("a valid name")
            .with_modified(modified)
    }

    #[test]
    fn a_press_on_the_modified_header_starts_with_the_newest() {
        let sort = FileSort::name().pressed(SortKey::Modified);
        assert_eq!(sort.key(), SortKey::Modified);
        assert!(
            sort.descending(),
            "what changed last is what a reader looks for"
        );
        assert!(
            !sort.pressed(SortKey::Modified).descending(),
            "a second press reverses it"
        );
        assert!(
            !sort.pressed(SortKey::Size).descending(),
            "a size still starts with the smallest"
        );
    }

    #[test]
    fn the_modified_order_keeps_folders_first_and_unknown_times_last() {
        let mut rows = [
            stamped("old.txt", FileKind::File, Some(100)),
            stamped("unknown.txt", FileKind::File, None),
            stamped("new.txt", FileKind::File, Some(300)),
            stamped("dir", FileKind::Directory, Some(1)),
        ];
        let newest = FileSort::name().pressed(SortKey::Modified);
        rows.sort_by(|left, right| compare_entries(left, right, newest));
        let names: Vec<&str> = rows.iter().map(DirEntry::name).collect();
        assert_eq!(names, ["dir", "new.txt", "old.txt", "unknown.txt"]);

        let oldest = newest.pressed(SortKey::Modified);
        rows.sort_by(|left, right| compare_entries(left, right, oldest));
        let names: Vec<&str> = rows.iter().map(DirEntry::name).collect();
        assert_eq!(names, ["dir", "old.txt", "new.txt", "unknown.txt"]);
    }

    /// What a scripted connection holds.
    #[derive(Default)]
    struct FakeScript {
        sent: Vec<BrowseRequest>,
        answers: Vec<BrowseResponse>,
        accepting: bool,
    }

    /// A channel that records requests and answers the ones a test chooses.
    ///
    /// The handle is shared with the test, because an answer to a change arrives after
    /// the view exists: what the view does with it is the behaviour under test.
    #[derive(Clone, Default)]
    struct FakeChannel(std::sync::Arc<std::sync::Mutex<FakeScript>>);

    impl FakeChannel {
        fn accepting() -> Self {
            let channel = Self::default();
            channel.0.lock().expect("a free lock").accepting = true;
            channel
        }

        fn closed() -> Self {
            Self::default()
        }

        fn reply(&self, request: u64, path: &str, names: &[(&str, FileKind)]) {
            let entries = names
                .iter()
                .filter_map(|(name, kind)| DirEntry::new(*name, *kind, None))
                .collect();
            self.0
                .lock()
                .expect("a free lock")
                .answers
                .push(BrowseResponse::Listed {
                    request,
                    path: path.to_owned(),
                    entries,
                });
        }

        fn changed(&self, request: u64, outcome: MutationOutcome) {
            self.0
                .lock()
                .expect("a free lock")
                .answers
                .push(BrowseResponse::Changed { request, outcome });
        }

        fn requests(&self) -> Vec<BrowseRequest> {
            self.0.lock().expect("a free lock").sent.clone()
        }
    }

    impl BrowseChannel for FakeChannel {
        fn send(&mut self, request: BrowseRequest) -> bool {
            let mut script = self.0.lock().expect("a free lock");
            if !script.accepting {
                return false;
            }
            script.sent.push(request);
            true
        }

        fn drain(&mut self) -> Vec<BrowseResponse> {
            std::mem::take(&mut self.0.lock().expect("a free lock").answers)
        }
    }

    fn views(session: SessionId, channel: FakeChannel) -> FileViews {
        let mut views = FileViews::new();
        views.attach(session, "web", "deploy@10.0.0.7:22", Box::new(channel));
        views
    }

    #[test]
    fn every_entry_kind_has_its_own_label() {
        use crate::app::copy::{CHINESE, ENGLISH};
        let kinds = [
            FileKind::Directory,
            FileKind::File,
            FileKind::Symlink,
            FileKind::Other,
        ];
        let mut labels: Vec<&str> = kinds
            .iter()
            .map(|kind| kind_label(*kind, &ENGLISH))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), kinds.len(), "each kind has its own label");
        assert_ne!(
            kind_label(FileKind::Directory, &ENGLISH),
            kind_label(FileKind::Directory, &CHINESE)
        );
    }

    #[test]
    fn opening_a_view_asks_for_the_login_directory_and_not_for_a_guess() {
        let session = SessionId::new();
        let mut views = views(session, FakeChannel::accepting());
        assert!(!views.is_open(session));
        assert!(views.open(session));
        let browser = views.browser(session).expect("the view is open");
        assert_eq!(browser.path(), None, "no path is invented before an answer");
        assert!(browser.state().is_loading());
        assert_eq!(browser.host(), "web");
        assert_eq!(browser.target(), "deploy@10.0.0.7:22");
    }

    #[test]
    fn a_listing_answer_becomes_visible_only_after_a_pump() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        channel.reply(1, "/home/deploy", &[("docs", FileKind::Directory)]);
        let mut views = FileViews::new();
        views.attach(session, "web", "deploy@10.0.0.7:22", Box::new(channel));
        views.open(session);

        assert!(views.pump(), "the answer changed the view");
        let browser = views.browser(session).expect("open");
        assert_eq!(browser.path(), Some("/home/deploy"));
        let listing = browser.state().listing().expect("a real listing");
        assert_eq!(listing.entries()[0].name(), "docs");
        assert!(!views.pump(), "nothing new arrived");
    }

    #[test]
    fn a_connection_that_cannot_take_the_request_says_so_at_once() {
        let session = SessionId::new();
        let views_channel = FakeChannel::closed();
        let mut views = views(session, views_channel);
        views.open(session);
        let browser = views.browser(session).expect("open");
        assert!(
            browser
                .state()
                .failure()
                .is_some_and(BrowseFailure::is_connection_lost),
            "a request that never left is reported instead of waiting: {:?}",
            browser.state()
        );
    }

    #[test]
    fn one_session_never_shows_another_sessions_listing() {
        let first = SessionId::new();
        let second = SessionId::new();
        let first_channel = FakeChannel::accepting();
        first_channel.reply(1, "/home/deploy/one", &[("one.txt", FileKind::File)]);
        let second_channel = FakeChannel::accepting();
        second_channel.reply(1, "/home/other/two", &[("two.txt", FileKind::File)]);

        let mut views = FileViews::new();
        views.attach(first, "web", "deploy@10.0.0.7:22", Box::new(first_channel));
        views.attach(second, "db", "deploy@10.0.0.8:22", Box::new(second_channel));
        views.open(first);
        views.open(second);
        views.pump();

        assert_eq!(
            views.browser(first).and_then(FileBrowser::path),
            Some("/home/deploy/one")
        );
        assert_eq!(
            views.browser(second).and_then(FileBrowser::path),
            Some("/home/other/two")
        );

        // Closing one view leaves the other exactly as it was.
        views.close(first);
        assert!(!views.is_open(first));
        assert!(views.is_open(second));
        views.detach(first);
        assert!(!views.can_browse(first));
        assert!(views.is_open(second));
    }

    #[test]
    fn navigation_from_a_real_listing_asks_the_right_path() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        channel.reply(
            1,
            "/home/deploy",
            &[
                ("文档 与 空格", FileKind::Directory),
                ("note.txt", FileKind::File),
            ],
        );
        let mut views = views(session, channel);
        views.open(session);
        views.pump();
        assert!(views.enter(session, "文档 与 空格"));
        assert!(
            !views.enter(session, "note.txt"),
            "a file is not opened as a directory"
        );
        let browser = views.browser(session).expect("open");
        assert!(browser.state().is_loading());
        assert_eq!(browser.path(), Some("/home/deploy/文档 与 空格"));
    }

    /// Opens one view whose directory is already listed by the server.
    fn listed_view(channel: &FakeChannel) {
        channel.reply(
            1,
            "/home/deploy",
            &[("docs", FileKind::Directory), ("notes.txt", FileKind::File)],
        );
    }

    #[test]
    fn a_change_needs_a_listing_and_is_followed_by_the_servers_next_listing() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        listed_view(&channel);
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(channel.clone()),
        );
        views.open(session);
        assert_eq!(
            views.create_directory(session, "logs"),
            Err(NameProblem::NoDirectory),
            "no directory was listed yet, so there is nowhere to create one"
        );
        views.pump();

        assert_eq!(views.create_directory(session, "日志"), Ok(()));
        let requests = channel.requests();
        assert_eq!(
            requests.last(),
            Some(&BrowseRequest::CreateDirectory {
                request: 2,
                directory: "/home/deploy".to_owned(),
                name: "日志".to_owned(),
            }),
            "the change names the directory on screen and the exact new name: {requests:?}"
        );
        assert!(
            views.browser(session).is_some_and(FileBrowser::is_changing),
            "the view knows a change is with the server"
        );
        assert!(
            views
                .browser(session)
                .is_some_and(|browser| browser.state().listing().is_some()),
            "the entries stay on screen while the change is in flight"
        );

        let outcome = MutationOutcome::new(
            shellune_core::MutationAction::CreateDirectory {
                path: "/home/deploy/日志".to_owned(),
            },
            vec![shellune_core::MutationStep::done(
                "/home/deploy/日志",
                Some(FileKind::Directory),
            )],
        );
        channel.changed(2, outcome.clone());
        assert!(views.pump(), "the report arrived");
        assert_eq!(views.outcome(session), Some(&outcome));
        assert!(
            !views.browser(session).is_some_and(FileBrowser::is_changing),
            "the change is answered"
        );
        let requests = channel.requests();
        assert_eq!(
            requests.last(),
            Some(&BrowseRequest::List {
                request: 3,
                path: "/home/deploy".to_owned(),
            }),
            "the directory is read again instead of being assumed: {requests:?}"
        );

        channel.reply(
            3,
            "/home/deploy",
            &[
                ("docs", FileKind::Directory),
                ("日志", FileKind::Directory),
                ("notes.txt", FileKind::File),
            ],
        );
        views.pump();
        let names: Vec<String> = views
            .browser(session)
            .and_then(|browser| browser.state().listing())
            .map(|listing| {
                listing
                    .entries()
                    .iter()
                    .map(|entry| entry.name().to_owned())
                    .collect()
            })
            .expect("a listing is shown");
        assert_eq!(
            names,
            vec!["docs".to_owned(), "日志".to_owned(), "notes.txt".to_owned()],
            "the list after the change is the server's own answer"
        );
    }

    #[test]
    fn a_name_the_listing_refuses_is_answered_without_a_request() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        listed_view(&channel);
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(channel.clone()),
        );
        views.open(session);
        views.pump();
        let sent = channel.requests().len();

        assert_eq!(
            views.delete(session, "not-listed"),
            Err(NameProblem::Missing)
        );
        assert_eq!(
            views.rename(session, "docs", "docs"),
            Err(NameProblem::Unchanged)
        );
        assert_eq!(
            views.rename(session, "docs", "notes.txt"),
            Err(NameProblem::Taken)
        );
        assert_eq!(
            channel.requests().len(),
            sent,
            "a name the listing already answers never reaches the server"
        );
    }

    #[test]
    fn a_delete_is_asked_for_with_the_kind_the_listing_reported() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        listed_view(&channel);
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(channel.clone()),
        );
        views.open(session);
        views.pump();

        assert_eq!(views.delete(session, "docs"), Ok(()));
        assert_eq!(
            channel.requests().last(),
            Some(&BrowseRequest::Delete {
                request: 2,
                path: "/home/deploy/docs".to_owned(),
                kind: FileKind::Directory,
            }),
            "the confirmation and the request name the same object"
        );
        assert_eq!(views.rename(session, "notes.txt", "说明.txt"), Ok(()));
        assert_eq!(
            channel.requests().last(),
            Some(&BrowseRequest::Rename {
                request: 3,
                from: "/home/deploy/notes.txt".to_owned(),
                to: "/home/deploy/说明.txt".to_owned(),
            }),
            "a rename keeps the entry in its own directory"
        );
    }

    #[test]
    fn forgetting_a_session_takes_its_view_and_its_answers_with_it() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        listed_view(&channel);
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(channel.clone()),
        );
        views.open(session);
        views.pump();
        assert!(views.is_open(session));

        assert!(views.detach(session), "the open view is reported as gone");
        assert!(!views.is_open(session), "no path survives the connection");
        assert!(!views.any_open(), "nothing is left to poll");
        assert_eq!(
            views.create_directory(session, "logs"),
            Err(NameProblem::NoDirectory),
            "a forgotten session cannot be changed"
        );
    }

    /// A directory of this process, removed when the test returns.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "shellune-files-{}-{}",
                std::process::id(),
                SessionId::new()
            ));
            std::fs::create_dir_all(&path).expect("a temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn opening_lists_both_sources_and_a_remote_failure_leaves_the_local_one() {
        let session = SessionId::new();
        let remote = FakeChannel::accepting();
        remote.reply(1, "/home/deploy", &[("notes.txt", FileKind::File)]);
        let local = FakeChannel::accepting();
        local.reply(1, "/tmp/work", &[("local.txt", FileKind::File)]);
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(remote.clone()),
        );
        views.attach_local_for_test(session, Box::new(local.clone()));
        assert!(views.open(session), "the authenticated session opens files");
        assert_eq!(
            remote.requests(),
            vec![BrowseRequest::Home { request: 1 }],
            "opening asks the existing connection for its home directory and nothing else"
        );
        views.pump();
        assert_eq!(
            views.browser(session).and_then(FileBrowser::path),
            Some("/home/deploy")
        );
        assert_eq!(
            views
                .browser_in(session, FileSource::Local)
                .map(FileBrowser::owner),
            views.browser(session).map(FileBrowser::owner),
            "both columns belong to the session that was already authenticated"
        );
        assert_ne!(
            views
                .browser_in(session, FileSource::Local)
                .map(FileBrowser::host),
            views.browser(session).map(FileBrowser::host),
            "the local machine is not labelled as the remote host"
        );
        assert_ne!(
            views
                .browser_in(session, FileSource::Local)
                .and_then(FileBrowser::path),
            views.browser(session).and_then(FileBrowser::path),
            "the local path is not the remote path"
        );
        assert_eq!(views.shown(session), Some(FileSource::Remote));
        assert!(views.show(session, FileSource::Local));
        assert_eq!(
            views.browser(session).and_then(FileBrowser::path),
            Some("/home/deploy"),
            "showing the local column does not retarget the remote one"
        );

        local
            .0
            .lock()
            .expect("a free lock")
            .answers
            .push(BrowseResponse::Failed {
                request: 2,
                failure: BrowseFailure::PermissionDenied {
                    path: "/tmp/secret".to_owned(),
                },
            });
        assert!(views.list_at(session, FileSource::Local, "/tmp/secret"));
        views.pump();
        assert!(
            views
                .browser_in(session, FileSource::Local)
                .and_then(|browser| browser.state().failure())
                .is_some_and(|failure| matches!(failure, BrowseFailure::PermissionDenied { .. })),
            "the local source reports the refusal"
        );
        assert_eq!(
            views.browser(session).and_then(FileBrowser::path),
            Some("/home/deploy"),
            "a local refusal does not replace the remote listing"
        );
        assert!(views.browser(session).unwrap().state().listing().is_some());
    }

    #[test]
    fn a_lost_remote_connection_does_not_clear_the_local_directory() {
        let session = SessionId::new();
        let remote = FakeChannel::closed();
        let local = FakeChannel::accepting();
        local.reply(1, "/tmp/work", &[("keep.txt", FileKind::File)]);
        let mut views = FileViews::new();
        views.attach(session, "web", "deploy@10.0.0.7:22", Box::new(remote));
        views.attach_local_for_test(session, Box::new(local));
        views.open(session);
        views.pump();
        assert!(
            views
                .browser(session)
                .and_then(|browser| browser.state().failure())
                .is_some_and(BrowseFailure::is_connection_lost),
            "the remote source says the connection is gone"
        );
        assert_eq!(
            views
                .browser_in(session, FileSource::Local)
                .and_then(FileBrowser::path),
            Some("/tmp/work"),
            "the local directory is still the one this computer listed"
        );
    }

    #[test]
    fn a_local_change_is_visible_only_after_the_directory_is_read_again() {
        let session = SessionId::new();
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), b"note").unwrap();
        let protocol = local_os_to_protocol(root.path());
        let remote = FakeChannel::accepting();
        remote.reply(1, "/home/deploy", &[("server.txt", FileKind::File)]);
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(remote.clone()),
        );
        views.open(session);
        assert!(views.list_at(session, FileSource::Local, &protocol));
        views.pump();
        let names = |views: &FileViews| {
            views
                .browser_in(session, FileSource::Local)
                .and_then(|browser| browser.state().listing())
                .map(|listing| {
                    listing
                        .entries()
                        .iter()
                        .map(|entry| entry.name().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        assert_eq!(names(&views), vec!["notes.txt".to_owned()]);
        assert!(
            views
                .browser(session)
                .and_then(|browser| browser.state().listing())
                .is_some_and(|listing| listing.entries()[0].name() == "server.txt"),
            "the remote listing arrived on its own"
        );

        assert_eq!(
            views.create_directory_in(session, FileSource::Local, "日志"),
            Ok(())
        );
        assert_eq!(
            names(&views),
            vec!["notes.txt".to_owned()],
            "the new folder is not shown before this computer lists the directory again"
        );
        // The change report and the following listing are two answers.
        views.pump();
        views.pump();
        assert!(
            names(&views).contains(&"日志".to_owned()),
            "the refreshed listing is what the disk holds: {:?}",
            names(&views)
        );
        assert!(root.path().join("日志").is_dir());

        assert_eq!(
            views.rename_in(session, FileSource::Local, "notes.txt", "说明.txt"),
            Ok(())
        );
        views.pump();
        views.pump();
        assert!(names(&views).contains(&"说明.txt".to_owned()));
        assert!(!root.path().join("notes.txt").exists());

        assert_eq!(views.delete_in(session, FileSource::Local, "日志"), Ok(()));
        views.pump();
        views.pump();
        assert!(
            !names(&views).contains(&"日志".to_owned()),
            "a deleted folder leaves the listing only after the refresh"
        );
        assert!(!root.path().join("日志").exists());
        assert_eq!(
            remote
                .requests()
                .iter()
                .filter(|request| !request.is_listing())
                .count(),
            0,
            "a local change is not sent to the server"
        );
    }

    #[test]
    fn an_empty_missing_or_non_directory_local_path_names_itself() {
        let session = SessionId::new();
        let root = TempDir::new();
        let empty = root.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let file = root.path().join("plain.txt");
        std::fs::write(&file, b"x").unwrap();
        let remote = FakeChannel::accepting();
        remote.reply(1, "/home/deploy", &[("server.txt", FileKind::File)]);
        let mut views = FileViews::new();
        views.attach(session, "web", "deploy@10.0.0.7:22", Box::new(remote));
        views.open(session);

        assert!(views.list_at(session, FileSource::Local, &local_os_to_protocol(&empty)));
        views.pump();
        assert!(
            views
                .browser_in(session, FileSource::Local)
                .and_then(|browser| browser.state().listing())
                .is_some_and(|listing| listing.is_empty()),
            "an empty directory is empty, not a failure"
        );

        let missing = local_os_to_protocol(&root.path().join("no-such"));
        assert!(views.list_at(session, FileSource::Local, &missing));
        views.pump();
        assert!(
            matches!(
                views
                    .browser_in(session, FileSource::Local)
                    .and_then(|browser| browser.state().failure()),
                Some(BrowseFailure::NotFound { .. })
            ),
            "a path that is not there says so"
        );
        assert!(
            views.browser(session).unwrap().state().listing().is_some(),
            "the remote listing survives a missing local path"
        );

        assert!(views.list_at(session, FileSource::Local, &local_os_to_protocol(&file)));
        views.pump();
        assert!(
            matches!(
                views
                    .browser_in(session, FileSource::Local)
                    .and_then(|browser| browser.state().failure()),
                Some(BrowseFailure::NotADirectory { .. })
            ),
            "a file is not listed as a directory"
        );
    }

    #[test]
    fn a_permission_error_is_not_reported_as_a_missing_path() {
        let failure = io_failure(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            "/secret",
        );
        assert!(
            matches!(failure, BrowseFailure::PermissionDenied { .. }),
            "permission and absence are different reasons: {failure:?}"
        );
        assert!(!failure.is_connection_lost());
    }

    #[test]
    fn cancelling_is_a_property_of_the_question_not_of_a_request() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        listed_view(&channel);
        let mut views = views(session, channel.clone());
        views.open(session);
        views.pump();
        let sent = channel.requests().len();
        // The dialog is what the window cancels. Building it and dropping it must
        // not be representable as a request, which is why delete is not called here.
        let dialog = FilesDialog::Delete {
            session,
            source: FileSource::Remote,
            entries: vec![DeleteTarget::new("docs", FileKind::Directory)],
            problem: None,
        };
        assert_eq!(dialog.source(), FileSource::Remote);
        drop(dialog);
        assert_eq!(channel.requests().len(), sent);
    }

    #[test]
    fn a_local_name_that_would_escape_is_refused_before_the_filesystem_is_touched() {
        let root = TempDir::new();
        let work = root.path().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(work.join("inside")).unwrap();
        let directory = local_os_to_protocol(&work);
        let from = join_path(&directory, "inside");
        // The shared POSIX check still allows a backslash: a remote name may contain one.
        assert!(shellune_core::check_name(r"..\outside").is_ok());
        let escapes = [r"..\outside", r"D:\abs", r"\Windows\shellune-escape"];
        for name in escapes {
            let created = create_local_directory(&directory, name);
            assert!(
                matches!(
                    &created,
                    Err(BrowseFailure::Refused { reason })
                        if reason == "the name leaves this directory"
                ),
                "{name} must be refused before create_dir, got {created:?}"
            );
            let renamed = rename_local(&from, &join_path(&directory, name));
            assert!(
                matches!(
                    &renamed,
                    Err(BrowseFailure::Refused { reason })
                        if reason == "the name leaves this directory"
                ),
                "{name} must be refused before rename, got {renamed:?}"
            );
        }
        assert!(
            work.join("inside").is_dir(),
            "the entry that would have been renamed is still in this directory"
        );
        assert!(
            !root.path().join("outside").exists(),
            "a parent-relative name must not be created beside the open directory"
        );
        let children: Vec<_> = std::fs::read_dir(&work)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(children, vec!["inside".to_owned()]);
        assert!(create_local_directory(&directory, "日志").is_ok());
        assert!(work.join("日志").is_dir());
        assert!(
            rename_local(
                &join_path(&directory, "日志"),
                &join_path(&directory, "说明")
            )
            .is_ok()
        );
        assert!(work.join("说明").is_dir());
        assert!(!work.join("日志").exists());
    }

    #[test]
    fn windows_drive_paths_round_trip_without_becoming_posix_roots() {
        use std::path::{Path, PathBuf};

        if !cfg!(windows) {
            let protocol = local_os_to_protocol(Path::new("/tmp/work"));
            assert_eq!(protocol, "/tmp/work");
            assert_eq!(
                local_protocol_to_os("/tmp/work").unwrap(),
                PathBuf::from("/tmp/work")
            );
            return;
        }
        let protocol = local_os_to_protocol(Path::new(r"C:\work\文档"));
        assert_eq!(protocol, "/C:/work/文档");
        assert_eq!(
            local_protocol_to_os(&protocol).unwrap(),
            PathBuf::from(r"C:\work\文档")
        );
        assert!(local_protocol_to_os("/").is_none());
        let crumbs = path_crumbs("/C:/work");
        assert_eq!(crumbs[0].path, "/");
        assert_eq!(crumbs.last().unwrap().path, "/C:/work");
    }

    /// A view whose first listing already arrived at `/home/deploy`.
    fn opened_at(session: SessionId, channel: &FakeChannel) -> FileViews {
        let mut views = views(session, channel.clone());
        channel.reply(1, "/home/deploy", &[("docs", FileKind::Directory)]);
        views.open(session);
        views.pump();
        views
    }

    #[test]
    fn a_view_opened_after_a_directory_report_starts_there() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = views(session, channel.clone());
        assert!(!views.note_terminal_directory(session, "/var/log", true));
        assert_eq!(
            channel.requests(),
            Vec::<BrowseRequest>::new(),
            "nothing is asked before the view exists"
        );

        views.open(session);
        match channel.requests().first() {
            Some(BrowseRequest::List { path, .. }) => assert_eq!(path, "/var/log"),
            other => panic!("the reported directory is asked for first, got {other:?}"),
        }
        channel.reply(1, "/var/log", &[("syslog", FileKind::File)]);
        views.pump();
        assert_eq!(
            views.browser(session).and_then(FileBrowser::path),
            Some("/var/log"),
            "the first listing is the directory the shell reported"
        );
        assert_eq!(
            views.follow_note(session),
            Some(FollowNote::Following {
                path: "/var/log".to_owned()
            })
        );
    }

    #[test]
    fn a_later_directory_report_moves_the_listing_of_its_own_session() {
        let first = SessionId::new();
        let second = SessionId::new();
        let first_channel = FakeChannel::accepting();
        let second_channel = FakeChannel::accepting();
        let mut views = opened_at(first, &first_channel);
        views.attach(
            second,
            "db",
            "deploy@10.0.0.8:22",
            Box::new(second_channel.clone()),
        );
        second_channel.reply(1, "/home/deploy", &[]);
        views.open(second);
        views.pump();

        // The active terminal of the first session changes directory.
        assert!(views.note_terminal_directory(first, "/var/log", true));
        let asked = first_channel.requests().last().cloned();
        match asked {
            Some(BrowseRequest::List { path, request }) => {
                assert_eq!(path, "/var/log");
                first_channel.reply(request, "/var/log", &[("syslog", FileKind::File)]);
            }
            other => panic!("a listing was asked for, got {other:?}"),
        }
        views.pump();
        assert_eq!(
            views.browser(first).and_then(FileBrowser::path),
            Some("/var/log")
        );
        assert_eq!(
            views.browser(second).and_then(FileBrowser::path),
            Some("/home/deploy"),
            "the other session's listing is untouched"
        );
    }

    #[test]
    fn browsing_by_hand_turns_following_off_until_the_switch_is_used_again() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = opened_at(session, &channel);

        // The user opens a row, which is browsing rather than following.
        assert!(views.enter(session, "docs"));
        assert!(!views.follows(session));
        assert_eq!(views.follow_note(session), Some(FollowNote::Manual));
        views.note_terminal_directory(session, "/var/log", true);

        // A shell that changes directory afterwards does not take the path back.
        let before = channel.requests().len();
        assert!(!views.note_terminal_directory(session, "/tmp", true));
        assert_eq!(
            channel.requests().len(),
            before,
            "a manual path is not overwritten by the next cd"
        );

        // Turning following on returns to the directory the shell last reported.
        assert!(views.set_follow(session, true));
        match channel.requests().last() {
            Some(BrowseRequest::List { path, .. }) => assert_eq!(path, "/tmp"),
            other => panic!("following returns to the reported directory, got {other:?}"),
        }
    }

    #[test]
    fn a_report_that_cannot_be_read_keeps_the_last_readable_directory() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = opened_at(session, &channel);
        views.note_terminal_directory(session, "/var/log", true);
        let request = match channel.requests().last() {
            Some(BrowseRequest::List { request, .. }) => *request,
            other => panic!("a listing was asked for, got {other:?}"),
        };
        channel.reply(request, "/var/log", &[("syslog", FileKind::File)]);
        views.pump();
        assert_eq!(
            views.browser(session).and_then(FileBrowser::path),
            Some("/var/log")
        );

        // The shell goes somewhere this account may not read.
        views.note_terminal_directory(session, "/root/private", true);
        let refused = match channel.requests().last() {
            Some(BrowseRequest::List { path, request }) => {
                assert_eq!(path, "/root/private");
                *request
            }
            other => panic!("the reported directory is asked for, got {other:?}"),
        };
        channel
            .0
            .lock()
            .expect("a free lock")
            .answers
            .push(BrowseResponse::Failed {
                request: refused,
                failure: BrowseFailure::PermissionDenied {
                    path: "/root/private".to_owned(),
                },
            });
        views.pump();

        assert_eq!(
            views.follow_note(session),
            Some(FollowNote::Unreadable {
                path: "/root/private".to_owned()
            })
        );
        // The column is back on the last directory the server really listed.
        match channel.requests().last() {
            Some(BrowseRequest::List { path, .. }) => assert_eq!(path, "/var/log"),
            other => panic!("the last readable directory is listed again, got {other:?}"),
        }
    }

    #[test]
    fn a_late_listing_does_not_put_an_older_directory_back() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = opened_at(session, &channel);
        views.note_terminal_directory(session, "/var/log", true);
        let first = match channel.requests().last() {
            Some(BrowseRequest::List { path, request, .. }) => {
                assert_eq!(path, "/var/log");
                *request
            }
            other => panic!("a listing was asked for, got {other:?}"),
        };
        // The shell moves again before the first answer arrives.
        views.note_terminal_directory(session, "/etc", true);
        let second = match channel.requests().last() {
            Some(BrowseRequest::List { path, request, .. }) => {
                assert_eq!(path, "/etc");
                *request
            }
            other => panic!("a listing was asked for, got {other:?}"),
        };
        assert!(second > first);

        // The answer for the directory the user has left arrives last.
        channel.reply(second, "/etc", &[("hosts", FileKind::File)]);
        views.pump();
        channel.reply(first, "/var/log", &[("syslog", FileKind::File)]);
        views.pump();

        assert_eq!(
            views.browser(session).and_then(FileBrowser::path),
            Some("/etc"),
            "a late listing for a directory the user left changes nothing"
        );
    }

    #[test]
    fn a_connection_that_starts_again_starts_without_the_old_directory() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = opened_at(session, &channel);
        views.note_terminal_directory(session, "/var/log", true);

        // A reconnect ends the old connection and attaches a new one to the same
        // session id, exactly as the window does.
        views.detach(session);
        let fresh = FakeChannel::accepting();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(fresh.clone()),
        );
        assert_eq!(
            views.follow_note(session),
            None,
            "no view is open, and nothing of the old report is left"
        );

        views.open(session);
        assert_eq!(
            fresh.requests().first(),
            Some(&BrowseRequest::Home { request: 1 }),
            "the new connection starts at the login directory it reports itself"
        );
    }

    #[test]
    fn a_report_from_a_pane_that_is_not_active_is_remembered_but_does_not_move_the_view() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = opened_at(session, &channel);
        let before = channel.requests().len();

        assert!(!views.note_terminal_directory(session, "/var/log", false));
        assert_eq!(
            channel.requests().len(),
            before,
            "a split pane that is not the active one does not move the listing"
        );

        assert!(views.set_follow(session, true));
        match channel.requests().last() {
            Some(BrowseRequest::List { path, .. }) => assert_eq!(path, "/var/log"),
            other => panic!("the switch goes to the last report, got {other:?}"),
        }
    }

    #[test]
    fn toggling_hidden_files_hides_and_restores_dotfiles_per_column() {
        let session = SessionId::new();
        let channel = FakeChannel::accepting();
        let mut views = FileViews::new();
        views.attach(
            session,
            "web",
            "deploy@10.0.0.7:22",
            Box::new(channel.clone()),
        );
        assert!(views.open(session));
        channel.reply(
            1,
            "/home/deploy",
            &[
                (".env", FileKind::File),
                (".ssh", FileKind::Directory),
                ("notes.txt", FileKind::File),
            ],
        );
        assert!(views.pump());

        assert!(views.show_hidden(session, FileSource::Remote));
        assert_eq!(views.rows(session, FileSource::Remote).len(), 3);
        assert_eq!(views.counts(session, FileSource::Remote), Some((3, 3)));

        assert!(!views.toggle_show_hidden(session, FileSource::Remote));
        assert!(views.show_hidden(session, FileSource::Local));
        let names: Vec<String> = views
            .rows(session, FileSource::Remote)
            .into_iter()
            .map(|entry| entry.name().to_owned())
            .collect();
        assert_eq!(names, vec!["notes.txt"]);
        assert_eq!(views.counts(session, FileSource::Remote), Some((1, 3)));

        assert!(views.toggle_show_hidden(session, FileSource::Remote));
        assert_eq!(views.rows(session, FileSource::Remote).len(), 3);
    }
}
