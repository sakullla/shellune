//! Browsing a remote directory (R9, R12).
//!
//! The model here is a small state machine with one job: it may only report a
//! directory listing that a server really returned for the request it is waiting
//! for. Three rules follow from that, and each of them is what keeps the file view
//! honest:
//!
//! * a browser belongs to one session ([SessionId]) and carries the host it was
//!   opened for, so two hosts can be browsed at once and neither can show the
//!   other's entries;
//! * every request has its own number, and a response for any other number is
//!   ignored - a late answer from a directory the user already left cannot replace
//!   what is on screen;
//! * a failure keeps the path it happened at instead of silently moving elsewhere,
//!   so "not found" and "permission denied" describe the path the user asked for.
//!
//! Paths are POSIX paths and travel as the exact strings the user and the server
//! use: nothing here decodes, escapes or upper-cases them, which is why a name with
//! Chinese characters or spaces keeps its meaning all the way from the directory
//! entry into the next request.
//!
//! Mutations follow the same three rules (R11). A name is checked against the listing
//! that is on screen before anything is sent, so a name that would mean another path
//! never reaches the server; a mutation answer carries a step per path the server
//! really reached, so a recursive delete reports each item it removed and each one it
//! could not; and after the answer the browser lists the directory again, because
//! the only trustworthy statement about the result is the server's own next listing.

use crate::ids::SessionId;

/// What one entry in a directory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// A directory, which can be opened.
    Directory,
    /// A regular file.
    File,
    /// A symbolic link, whose target the browser has not resolved.
    Symlink,
    /// Anything else the server reported: a socket, a device, a fifo.
    Other,
}

impl FileKind {
    /// Short label the view prints beside the name.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Directory => "Directory",
            Self::File => "File",
            Self::Symlink => "Link",
            Self::Other => "Other",
        }
    }

    /// Whether the entry is a directory the user can open.
    #[must_use]
    pub fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// One entry of a directory, exactly as the server named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    name: String,
    kind: FileKind,
    size: Option<u64>,
}

impl DirEntry {
    /// An entry, or None when the name cannot be one.
    ///
    /// A directory listing never contains "." or ".." as entries of its own - those
    /// are the browser's own navigation - and a name with a separator in it would
    /// turn "open this entry" into "open some other path", so both are refused
    /// instead of being shown.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: FileKind, size: Option<u64>) -> Option<Self> {
        let name = name.into();
        if !is_single_name(&name) {
            return None;
        }
        Some(Self { name, kind, size })
    }

    /// The name the server reported, unchanged.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the entry is.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        self.kind
    }

    /// Size in bytes, when the server reported one.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// Whether opening this entry lists another directory.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.kind.is_directory()
    }
}

/// The entries of one directory, in the order the view lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryListing {
    path: String,
    entries: Vec<DirEntry>,
}

impl DirectoryListing {
    /// A listing of one path.
    ///
    /// The order is decided here and not by the server: directories first, then
    /// names without regard to case, then the exact name. A server orders entries
    /// differently on different file systems, and a list that reshuffles itself
    /// between two reads of the same directory is hard to use.
    #[must_use]
    pub fn new(path: impl AsRef<str>, entries: Vec<DirEntry>) -> Self {
        let mut entries = entries;
        entries.sort_by(|left, right| {
            right
                .is_directory()
                .cmp(&left.is_directory())
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });
        Self {
            path: normalize_path(path.as_ref()),
            entries,
        }
    }

    /// The absolute path this listing belongs to.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The entries, in view order.
    #[must_use]
    pub fn entries(&self) -> &[DirEntry] {
        &self.entries
    }

    /// Whether the directory really is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Why a directory could not be listed.
///
/// The variants are the situations the user can tell apart and act on differently:
/// a server without SFTP needs an administrator, a wrong path needs another path, a
/// refused directory needs different permissions, and a lost connection needs the
/// session to be looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseFailure {
    /// SFTP cannot be used on this connection at all.
    Unavailable {
        /// What the server or the protocol said, so the reason is specific.
        reason: String,
    },
    /// The path does not exist.
    NotFound {
        /// The path that was asked for.
        path: String,
    },
    /// The path is not a directory, so it has no listing.
    NotADirectory {
        /// The path that was asked for.
        path: String,
    },
    /// The server refused to let this user read the directory.
    PermissionDenied {
        /// The path that was asked for.
        path: String,
    },
    /// The path already exists, so the change that needed a free name cannot happen.
    AlreadyExists {
        /// The path that is already taken.
        path: String,
    },
    /// The connection ended, or was never usable for this request.
    ConnectionLost {
        /// What the transport reported.
        reason: String,
    },
    /// The server refused the request for a reason of its own.
    Refused {
        /// The reason the server gave.
        reason: String,
    },
}

impl BrowseFailure {
    /// Whether the failure says the connection is gone rather than the path.
    #[must_use]
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::ConnectionLost { .. })
    }

    /// The path the failure is about, when it is about one.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::NotFound { path }
            | Self::NotADirectory { path }
            | Self::PermissionDenied { path }
            | Self::AlreadyExists { path } => Some(path),
            _ => None,
        }
    }
}

impl std::fmt::Display for BrowseFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { reason } => write!(
                formatter,
                "SFTP is not available on this server, so its files cannot be browsed: {reason}"
            ),
            Self::NotFound { path } => write!(formatter, "{path} does not exist on the server"),
            Self::NotADirectory { path } => write!(formatter, "{path} is not a directory"),
            // The same variant is returned by listings, creates, renames and deletes,
            // so the sentence must not claim one of them: "read" here would describe a
            // delete as something it never was.
            Self::PermissionDenied { path } => {
                write!(formatter, "the server refused access to {path}")
            }
            Self::AlreadyExists { path } => {
                write!(formatter, "{path} already exists on the server")
            }
            Self::ConnectionLost { reason } => {
                write!(formatter, "the connection was lost: {reason}")
            }
            Self::Refused { reason } => {
                write!(formatter, "the server refused the request: {reason}")
            }
        }
    }
}

/// What the browser currently knows about the directory it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseState {
    /// A request is in flight and nothing is known about its answer yet.
    Loading,
    /// A real listing.
    Ready(DirectoryListing),
    /// The last request failed.
    Failed(BrowseFailure),
}

impl BrowseState {
    /// Whether a request is in flight.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    /// The listing, when the last request succeeded.
    #[must_use]
    pub fn listing(&self) -> Option<&DirectoryListing> {
        match self {
            Self::Ready(listing) => Some(listing),
            _ => None,
        }
    }

    /// The failure, when the last request failed.
    #[must_use]
    pub fn failure(&self) -> Option<&BrowseFailure> {
        match self {
            Self::Failed(failure) => Some(failure),
            _ => None,
        }
    }
}

/// What the browser asks the connection to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseRequest {
    /// List the directory the login starts in.
    Home {
        /// Request number the answer must carry.
        request: u64,
    },
    /// List one absolute path.
    List {
        /// Request number the answer must carry.
        request: u64,
        /// The absolute path to list.
        path: String,
    },
    /// Create one directory inside the directory on screen.
    CreateDirectory {
        /// Request number the answer must carry.
        request: u64,
        /// The directory the new one goes into.
        directory: String,
        /// The single name the new directory gets.
        name: String,
    },
    /// Rename one entry of the directory on screen, without moving it elsewhere.
    Rename {
        /// Request number the answer must carry.
        request: u64,
        /// The full path that is renamed.
        from: String,
        /// The full path it becomes; same directory, new name.
        to: String,
    },
    /// Delete one entry of the directory on screen, with everything inside it.
    Delete {
        /// Request number the answer must carry.
        request: u64,
        /// The full path that is deleted.
        path: String,
        /// What the entry is, as the listing reported it.
        kind: FileKind,
    },
}

impl BrowseRequest {
    /// The number the answer must carry.
    #[must_use]
    pub fn request(&self) -> u64 {
        match self {
            Self::Home { request }
            | Self::List { request, .. }
            | Self::CreateDirectory { request, .. }
            | Self::Rename { request, .. }
            | Self::Delete { request, .. } => *request,
        }
    }

    /// The path that was asked for, when one was named.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Home { .. } => None,
            Self::List { path, .. } => Some(path),
            // A mutation names a directory and a name separately, so there is no one
            // path this request is "about" until the server answers; the action
            // carries the target.
            Self::CreateDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => None,
        }
    }

    /// Whether this request asks for a listing rather than a change.
    #[must_use]
    pub fn is_listing(&self) -> bool {
        matches!(self, Self::Home { .. } | Self::List { .. })
    }
}

/// What a change asked the server to do.
///
/// The action names the exact target - the full path of a new directory, both names
/// of a rename, the full path and kind of a delete - so a confirmation, a report and a
/// test all talk about the same object instead of about "the entry".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationAction {
    /// Create one directory at this full path.
    CreateDirectory {
        /// The path the new directory gets.
        path: String,
    },
    /// Rename one entry, keeping it in the same directory.
    Rename {
        /// The full path that is renamed.
        from: String,
        /// The full path it becomes.
        to: String,
    },
    /// Delete one entry, and everything inside it when it is a directory.
    Delete {
        /// The full path that is deleted.
        path: String,
        /// What the entry was, as the listing reported it.
        kind: FileKind,
    },
}

impl MutationAction {
    /// The path the change is about: what is created, renamed or deleted.
    #[must_use]
    pub fn target(&self) -> &str {
        match self {
            Self::CreateDirectory { path } | Self::Delete { path, .. } => path,
            Self::Rename { from, .. } => from,
        }
    }

    /// Whether the action may reach more than the one path it names.
    #[must_use]
    pub fn is_recursive(&self) -> bool {
        matches!(
            self,
            Self::Delete {
                kind: FileKind::Directory,
                ..
            }
        )
    }
}

/// One path a change really reached, and what the server did with it.
///
/// A mutation that failed entirely still reports one step, so "nothing happened" and
/// "everything happened" never look the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationStep {
    path: String,
    kind: Option<FileKind>,
    failure: Option<BrowseFailure>,
}

impl MutationStep {
    /// A path the server changed.
    #[must_use]
    pub fn done(path: impl Into<String>, kind: Option<FileKind>) -> Self {
        Self {
            path: path.into(),
            kind,
            failure: None,
        }
    }

    /// A path the server refused, with the reason it gave.
    #[must_use]
    pub fn failed(path: impl Into<String>, kind: Option<FileKind>, failure: BrowseFailure) -> Self {
        Self {
            path: path.into(),
            kind,
            failure: Some(failure),
        }
    }

    /// The path this step is about.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the path was, when the change knew.
    #[must_use]
    pub fn kind(&self) -> Option<FileKind> {
        self.kind
    }

    /// Why this path was not changed, when it was not.
    #[must_use]
    pub fn failure(&self) -> Option<&BrowseFailure> {
        self.failure.as_ref()
    }

    /// Whether the server really changed this path.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.failure.is_none()
    }
}

/// What one change really did: every path it reached, in the order it reached them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationOutcome {
    action: MutationAction,
    steps: Vec<MutationStep>,
}

impl MutationOutcome {
    /// The result of a change, from the steps the server reported.
    #[must_use]
    pub fn new(action: MutationAction, steps: Vec<MutationStep>) -> Self {
        Self { action, steps }
    }

    /// A change that never reached the path it names.
    #[must_use]
    pub fn refused(action: MutationAction, failure: BrowseFailure) -> Self {
        let path = action.target().to_owned();
        let kind = match &action {
            MutationAction::CreateDirectory { .. } => Some(FileKind::Directory),
            MutationAction::Delete { kind, .. } => Some(*kind),
            MutationAction::Rename { .. } => None,
        };
        Self {
            action,
            steps: vec![MutationStep::failed(path, kind, failure)],
        }
    }

    /// What was asked.
    #[must_use]
    pub fn action(&self) -> &MutationAction {
        &self.action
    }

    /// Every path the change reached, removed first for a recursive delete.
    #[must_use]
    pub fn steps(&self) -> &[MutationStep] {
        &self.steps
    }

    /// Whether the server changed everything the change asked for.
    ///
    /// A change that reached nothing at all is not a success: "no path was touched" is
    /// exactly what an answer to a request nobody served looks like.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(MutationStep::is_done)
    }

    /// How many paths the server changed.
    #[must_use]
    pub fn changed(&self) -> usize {
        self.steps.iter().filter(|step| step.is_done()).count()
    }

    /// How many paths the server refused.
    #[must_use]
    pub fn refused_paths(&self) -> usize {
        self.steps.len() - self.changed()
    }

    /// The paths the server did not change, with the reason for each.
    pub fn failures(&self) -> impl Iterator<Item = &MutationStep> {
        self.steps.iter().filter(|step| step.failure.is_some())
    }

    /// The first reason the server gave, when it refused anything.
    #[must_use]
    pub fn first_failure(&self) -> Option<&BrowseFailure> {
        self.steps.iter().find_map(|step| step.failure.as_ref())
    }
}

impl std::fmt::Display for MutationOutcome {
    /// One line about the whole change; the steps carry the details.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.succeeded() {
            return write!(
                formatter,
                "{} changed {} path(s) on the server",
                self.action.target(),
                self.steps.len()
            );
        }
        write!(
            formatter,
            "{} of {} path(s) under {} were not changed",
            self.refused_paths(),
            self.steps.len(),
            self.action.target()
        )?;
        if let Some(failure) = self.first_failure() {
            write!(formatter, ": {failure}")?;
        }
        Ok(())
    }
}

/// Why a name cannot be used for a change, decided before anything is sent.
///
/// Each of these would otherwise turn into a request about a different path than the
/// one the user sees, or a request the server would only answer with a bare failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameProblem {
    /// Nothing is on screen to change, so there is no directory to change it in.
    NoDirectory,
    /// The name is empty.
    Empty,
    /// The name is "." or "..", which are navigation rather than names.
    Navigation,
    /// The name holds a separator, so it would mean another path entirely.
    Separator,
    /// An entry with that name is already in the listing on screen.
    Taken,
    /// The entry being changed is not in the listing on screen.
    Missing,
    /// A rename to the same name changes nothing.
    Unchanged,
}

impl std::fmt::Display for NameProblem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDirectory => write!(formatter, "no directory is open to change"),
            Self::Empty => write!(formatter, "a name cannot be empty"),
            Self::Navigation => write!(formatter, "the names \".\" and \"..\" are navigation"),
            Self::Separator => write!(
                formatter,
                "a name cannot contain a separator; use one directory at a time"
            ),
            Self::Taken => write!(formatter, "an entry with that name already exists here"),
            Self::Missing => write!(formatter, "that entry is not in this directory"),
            Self::Unchanged => write!(formatter, "the name did not change"),
        }
    }
}

/// What the connection answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseResponse {
    /// A directory was read.
    Listed {
        /// The request this answers.
        request: u64,
        /// The absolute path that was read.
        path: String,
        /// Its entries.
        entries: Vec<DirEntry>,
    },
    /// The request failed.
    Failed {
        /// The request this answers.
        request: u64,
        /// Why it failed.
        failure: BrowseFailure,
    },
    /// A change was attempted, and this is every path it reached.
    ///
    /// It is an answer even when the change failed: the report names the paths and the
    /// reasons, so the view can say what really happened instead of only that
    /// something did not work.
    Changed {
        /// The request this answers.
        request: u64,
        /// What the change really did.
        outcome: MutationOutcome,
    },
}

impl BrowseResponse {
    /// The request this answers.
    #[must_use]
    pub fn request(&self) -> u64 {
        match self {
            Self::Listed { request, .. }
            | Self::Failed { request, .. }
            | Self::Changed { request, .. } => *request,
        }
    }

    /// The change this answer reports, when it reports one.
    #[must_use]
    pub fn outcome(&self) -> Option<&MutationOutcome> {
        match self {
            Self::Changed { outcome, .. } => Some(outcome),
            _ => None,
        }
    }
}

/// The directory view of one session.
#[derive(Debug)]
pub struct FileBrowser {
    owner: SessionId,
    host: String,
    target: String,
    path: Option<String>,
    state: BrowseState,
    outstanding: Option<u64>,
    next_request: u64,
    /// What the last change really did, until another one is attempted.
    outcome: Option<MutationOutcome>,
    /// Whether the request being waited for is a change rather than a listing.
    changing: bool,
}

impl FileBrowser {
    /// A browser for one session, before anything was listed.
    ///
    /// The host and the target are kept with the browser so a view cannot show a
    /// path without saying which machine it belongs to.
    #[must_use]
    pub fn new(owner: SessionId, host: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            owner,
            host: host.into(),
            target: target.into(),
            path: None,
            state: BrowseState::Loading,
            outstanding: None,
            next_request: 1,
            outcome: None,
            changing: false,
        }
    }

    /// The session this browser belongs to.
    #[must_use]
    pub fn owner(&self) -> SessionId {
        self.owner
    }

    /// The host the browser shows, as the user sees it.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The address the session talks to.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The path the current state belongs to, once one is known.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// What is known about that path.
    #[must_use]
    pub fn state(&self) -> &BrowseState {
        &self.state
    }

    /// Whether the browser is waiting for an answer.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        self.outstanding.is_some()
    }

    /// Whether the answer being waited for is a change, not a listing.
    ///
    /// It is what the view says while a rename or a delete is with the server: the
    /// entries are still there to read, so silence would look like nothing happened.
    #[must_use]
    pub fn is_changing(&self) -> bool {
        self.changing
    }

    /// Asks for the directory the login starts in.
    pub fn begin_home(&mut self) -> BrowseRequest {
        let request = self.begin();
        BrowseRequest::Home { request }
    }

    /// Asks again for the directory currently shown, or the login directory.
    pub fn refresh(&mut self) -> BrowseRequest {
        match self.path.clone() {
            Some(path) => self.begin_list(path),
            None => self.begin_home(),
        }
    }

    /// Asks for one path.
    pub fn begin_list(&mut self, path: impl Into<String>) -> BrowseRequest {
        let path = normalize_path(&path.into());
        let request = self.begin();
        self.path = Some(path.clone());
        BrowseRequest::List { request, path }
    }

    /// Opens one entry of the current listing.
    ///
    /// Returns None when the entry is not a directory or is not in the listing on
    /// screen: a view can only open what the server really reported, and a name is
    /// never joined into a path unless the listing carried it.
    pub fn open(&mut self, name: &str) -> Option<BrowseRequest> {
        let path = self.path.clone()?;
        let BrowseState::Ready(listing) = &self.state else {
            return None;
        };
        let entry = listing
            .entries()
            .iter()
            .find(|entry| entry.name() == name)?;
        if !entry.is_directory() {
            return None;
        }
        Some(self.begin_list(join_path(&path, entry.name())))
    }

    /// Goes to the parent of the directory shown.
    ///
    /// None at the root, and while no path is known: there is nowhere above
    /// either, and guessing one would list a directory the user did not ask for.
    pub fn up(&mut self) -> Option<BrowseRequest> {
        let path = self.path.clone()?;
        let parent = parent_path(&path)?;
        Some(self.begin_list(parent))
    }

    /// What the last change really did, until another attempt replaces it.
    #[must_use]
    pub fn last_outcome(&self) -> Option<&MutationOutcome> {
        self.outcome.as_ref()
    }

    /// The directory the listing on screen belongs to, when there is one.
    ///
    /// A change is only ever asked for inside a directory the server really listed:
    /// before that answer there is no path to put a new entry into.
    #[must_use]
    pub fn current_directory(&self) -> Option<&str> {
        match &self.state {
            BrowseState::Ready(listing) => Some(listing.path()),
            _ => None,
        }
    }

    /// The entry with this name, as the listing on screen reported it.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&DirEntry> {
        match &self.state {
            BrowseState::Ready(listing) => {
                listing.entries().iter().find(|entry| entry.name() == name)
            }
            _ => None,
        }
    }

    /// Asks for a new directory called name inside the directory on screen.
    ///
    /// The name is checked here, before the request exists: a name with a separator
    /// would create a path the user did not name, and a name that is already there
    /// would be a request the server can only refuse. Nothing is sent in either case,
    /// which is why the caller can show the reason without a round trip.
    pub fn create_directory(&mut self, name: &str) -> Result<BrowseRequest, NameProblem> {
        let directory = self
            .current_directory()
            .ok_or(NameProblem::NoDirectory)?
            .to_owned();
        self.check_free_name(name)?;
        let request = self.take_number();
        self.outcome = None;
        self.changing = true;
        Ok(BrowseRequest::CreateDirectory {
            request,
            directory,
            name: name.to_owned(),
        })
    }

    /// Asks to rename the entry called name to new_name, in place.
    pub fn rename(&mut self, name: &str, new_name: &str) -> Result<BrowseRequest, NameProblem> {
        let directory = self
            .current_directory()
            .ok_or(NameProblem::NoDirectory)?
            .to_owned();
        if self.entry(name).is_none() {
            return Err(NameProblem::Missing);
        }
        if name == new_name {
            return Err(NameProblem::Unchanged);
        }
        self.check_free_name(new_name)?;
        let from = join_path(&directory, name);
        let to = join_path(&directory, new_name);
        let request = self.take_number();
        self.outcome = None;
        self.changing = true;
        Ok(BrowseRequest::Rename { request, from, to })
    }

    /// Asks to delete the entry called name, with everything inside it.
    ///
    /// The kind travels with the request because the confirmation has to say what is
    /// being deleted - a directory takes its contents with it, a file does not - and
    /// because a second read of the name could be about a different entry.
    pub fn delete(&mut self, name: &str) -> Result<BrowseRequest, NameProblem> {
        let directory = self
            .current_directory()
            .ok_or(NameProblem::NoDirectory)?
            .to_owned();
        let entry = self.entry(name).ok_or(NameProblem::Missing)?;
        let path = join_path(&directory, entry.name());
        let kind = entry.kind();
        let request = self.take_number();
        self.outcome = None;
        self.changing = true;
        Ok(BrowseRequest::Delete {
            request,
            path,
            kind,
        })
    }

    /// Checks that a name can become one new entry of the directory on screen.
    fn check_free_name(&self, name: &str) -> Result<(), NameProblem> {
        check_name(name)?;
        if self.entry(name).is_some() {
            return Err(NameProblem::Taken);
        }
        Ok(())
    }

    /// Applies an answer, returning whether it belonged to the open request.
    ///
    /// An answer for another request is dropped: it belongs to a directory the user
    /// has already left, to a request this browser never made, or to a change that was
    /// replaced by a newer one. That single check is what keeps a late answer from
    /// overwriting the listing on screen.
    pub fn apply(&mut self, response: BrowseResponse) -> bool {
        if self.outstanding != Some(response.request()) {
            return false;
        }
        self.outstanding = None;
        self.changing = false;
        match response {
            BrowseResponse::Listed { path, entries, .. } => {
                let listing = DirectoryListing::new(path, entries);
                self.path = Some(listing.path().to_owned());
                self.state = BrowseState::Ready(listing);
            }
            BrowseResponse::Failed { failure, .. } => {
                // The failure names the path it is about, so the view keeps pointing
                // at the directory that could not be read instead of pretending the
                // user is somewhere else.
                if let Some(path) = failure.path() {
                    self.path = Some(normalize_path(path));
                }
                self.state = BrowseState::Failed(failure);
            }
            BrowseResponse::Changed { outcome, .. } => {
                // The listing on screen stays until the server's next listing arrives:
                // the report is what the change did, not what the directory now holds.
                self.outcome = Some(outcome);
            }
        }
        true
    }

    /// Numbers the next request and marks the browser as waiting for a listing.
    fn begin(&mut self) -> u64 {
        let request = self.take_number();
        self.state = BrowseState::Loading;
        request
    }

    /// Numbers a request that leaves the listing on screen alone.
    ///
    /// A change does not un-list the directory it happens in: the entries keep their
    /// places while the answer is missing, so a click cannot open a row that vanished.
    fn take_number(&mut self) -> u64 {
        let request = self.next_request;
        self.next_request += 1;
        self.outstanding = Some(request);
        request
    }
}

/// Whether a string can be one entry name.
///
/// A name with a separator would turn "open this entry" into "open another path",
/// and "." or ".." are the browser's navigation rather than entries of a listing.
#[must_use]
pub fn is_single_name(name: &str) -> bool {
    check_name(name).is_ok()
}

/// Checks a name that is about to become one entry of a directory.
///
/// The reason is a value rather than a bool because the view shows it: a user who
/// typed a name with a slash in it needs to read why nothing was sent, and "that is
/// not a valid name" is not a reason.
pub fn check_name(name: &str) -> Result<(), NameProblem> {
    if name.is_empty() {
        return Err(NameProblem::Empty);
    }
    if name == "." || name == ".." {
        return Err(NameProblem::Navigation);
    }
    if name.contains('/') || name.contains('\0') {
        return Err(NameProblem::Separator);
    }
    Ok(())
}

/// An absolute POSIX path with dots resolved and repeated separators folded.
///
/// Nothing else is touched: a name keeps its exact characters, so a directory
/// called 文档 与 空格 is requested and compared as that string.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return "/".to_owned();
    }
    let mut normalized = String::with_capacity(path.len() + 1);
    for part in parts {
        normalized.push('/');
        normalized.push_str(part);
    }
    normalized
}

/// The path of one entry inside a directory.
#[must_use]
pub fn join_path(directory: &str, name: &str) -> String {
    if directory == "/" {
        return format!("/{name}");
    }
    format!("{}/{}", directory.trim_end_matches('/'), name)
}

/// The directory above one path, or None at the root.
#[must_use]
pub fn parent_path(path: &str) -> Option<String> {
    let path = normalize_path(path);
    if path == "/" {
        return None;
    }
    let index = path.rfind('/')?;
    if index == 0 {
        Some("/".to_owned())
    } else {
        Some(path[..index].to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: FileKind) -> DirEntry {
        DirEntry::new(name, kind, None).expect("a valid entry name")
    }

    fn browser() -> FileBrowser {
        FileBrowser::new(SessionId::new(), "web", "deploy@10.0.0.7:22")
    }

    fn listed(request: u64, path: &str, entries: Vec<DirEntry>) -> BrowseResponse {
        BrowseResponse::Listed {
            request,
            path: path.to_owned(),
            entries,
        }
    }

    #[test]
    fn a_path_keeps_chinese_characters_and_spaces_exactly() {
        let path = "/home/deploy/文档 与 空格";
        assert_eq!(normalize_path(path), path);
        assert_eq!(join_path("/home/deploy", "文档 与 空格"), path);
        assert_eq!(parent_path(path).as_deref(), Some("/home/deploy"));
        assert!(is_single_name("文档 与 空格"));
    }

    #[test]
    fn a_path_is_resolved_without_decoding_anything_else() {
        assert_eq!(normalize_path("/a//b/./c/../d/"), "/a/b/d");
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("../../.."), "/");
        assert_eq!(normalize_path("/a b/./"), "/a b");
    }

    #[test]
    fn the_parent_of_a_root_directory_is_nowhere() {
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path("/home"), Some("/".to_owned()));
        assert_eq!(parent_path("/home/deploy"), Some("/home".to_owned()));
    }

    #[test]
    fn names_that_would_change_the_path_are_not_entries() {
        assert!(DirEntry::new(".", FileKind::Directory, None).is_none());
        assert!(DirEntry::new("..", FileKind::Directory, None).is_none());
        assert!(DirEntry::new("a/b", FileKind::File, None).is_none());
        assert!(DirEntry::new("", FileKind::File, None).is_none());
        assert!(DirEntry::new("ok name", FileKind::File, None).is_some());
    }

    #[test]
    fn directories_come_first_in_a_stable_order() {
        let listing = DirectoryListing::new(
            "/home/deploy",
            vec![
                entry("zeta.txt", FileKind::File),
                entry("Alpha", FileKind::Directory),
                entry("beta", FileKind::Directory),
                entry("archive.txt", FileKind::File),
            ],
        );
        let names: Vec<&str> = listing.entries().iter().map(|entry| entry.name()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "archive.txt", "zeta.txt"]);
        assert_eq!(listing.path(), "/home/deploy");
    }

    #[test]
    fn opening_an_entry_lists_the_directory_the_server_reported() {
        let mut browser = browser();
        let home = browser.begin_home();
        assert_eq!(home, BrowseRequest::Home { request: 1 });
        assert!(browser.state().is_loading(), "nothing is known yet");
        assert!(browser.apply(listed(
            1,
            "/home/deploy",
            vec![
                entry("文档 与 空格", FileKind::Directory),
                entry("notes.txt", FileKind::File),
            ],
        )));

        let request = browser.open("文档 与 空格").expect("a directory opens");
        assert_eq!(
            request,
            BrowseRequest::List {
                request: 2,
                path: "/home/deploy/文档 与 空格".to_owned(),
            }
        );
        assert!(
            browser.open("notes.txt").is_none(),
            "a file has no listing to open"
        );
        assert!(
            browser.open("not-listed").is_none(),
            "only entries the server reported can be opened"
        );
    }

    #[test]
    fn the_parent_comes_from_the_path_that_is_really_open() {
        let mut browser = browser();
        browser.begin_home();
        browser.apply(listed(1, "/home/deploy", Vec::new()));
        let up = browser.up().expect("the home directory has a parent");
        assert_eq!(
            up,
            BrowseRequest::List {
                request: 2,
                path: "/home".to_owned(),
            }
        );
        browser.apply(listed(2, "/home", Vec::new()));
        browser.begin_list("/");
        browser.apply(listed(3, "/", Vec::new()));
        assert!(browser.up().is_none(), "the root has nowhere above it");
    }

    #[test]
    fn a_response_for_another_request_never_replaces_the_listing() {
        let mut browser = browser();
        browser.begin_home();
        browser.apply(listed(1, "/home/deploy", vec![entry("a", FileKind::File)]));
        browser.begin_list("/home/deploy/文档");

        assert!(!browser.apply(listed(
            1,
            "/elsewhere",
            vec![entry("stale", FileKind::File)]
        )));
        assert!(browser.state().is_loading());
        assert_eq!(browser.path(), Some("/home/deploy/文档"));

        assert!(browser.apply(BrowseResponse::Failed {
            request: 2,
            failure: BrowseFailure::NotFound {
                path: "/home/deploy/文档".to_owned(),
            },
        }));
        assert_eq!(
            browser.state().failure(),
            Some(&BrowseFailure::NotFound {
                path: "/home/deploy/文档".to_owned(),
            })
        );
    }

    #[test]
    fn a_failure_names_the_path_it_happened_at_and_is_not_a_listing() {
        let mut browser = browser();
        browser.begin_home();
        browser.apply(BrowseResponse::Failed {
            request: 1,
            failure: BrowseFailure::PermissionDenied {
                path: "/home/deploy/secret".to_owned(),
            },
        });
        assert_eq!(browser.path(), Some("/home/deploy/secret"));
        assert!(browser.state().listing().is_none());
        let failure = browser.state().failure().expect("a failure is reported");
        assert!(!failure.is_connection_lost());
        assert!(failure.to_string().contains("/home/deploy/secret"));
        assert!(
            BrowseFailure::Unavailable {
                reason: "the server refused the sftp subsystem".to_owned()
            }
            .to_string()
            .contains("SFTP"),
            "the missing subsystem is named"
        );
    }

    #[test]
    fn two_browsers_of_two_sessions_never_share_a_path_or_a_result() {
        let mut first = FileBrowser::new(SessionId::new(), "web", "deploy@10.0.0.7:22");
        let mut second = FileBrowser::new(SessionId::new(), "db", "deploy@10.0.0.8:22");
        first.begin_home();
        first.apply(listed(1, "/home/deploy/srv", Vec::new()));
        second.begin_home();
        assert_eq!(second.path(), None, "the second host starts nowhere");
        assert!(
            !second.apply(listed(2, "/home/deploy/srv", Vec::new())),
            "the second host is waiting for its own first request, not this one"
        );
        assert_eq!(second.path(), None);

        second.apply(listed(1, "/home/other", Vec::new()));
        assert_eq!(second.path(), Some("/home/other"));
        assert_eq!(
            first.path(),
            Some("/home/deploy/srv"),
            "the first host keeps the directory it really listed"
        );
        assert_ne!(first.owner(), second.owner());
        assert_ne!(first.host(), second.host());
    }

    /// A browser that really listed a directory with a few entries.
    fn listed_browser() -> FileBrowser {
        let mut browser = browser();
        browser.begin_home();
        browser.apply(listed(
            1,
            "/home/deploy",
            vec![
                entry("docs", FileKind::Directory),
                entry("notes.txt", FileKind::File),
            ],
        ));
        browser
    }

    #[test]
    fn a_new_directory_is_asked_for_inside_the_listing_and_by_its_own_name() {
        let mut browser = listed_browser();
        let request = browser.create_directory("文档 新").expect("a free name");
        assert_eq!(
            request,
            BrowseRequest::CreateDirectory {
                request: 2,
                directory: "/home/deploy".to_owned(),
                name: "文档 新".to_owned(),
            }
        );
        assert!(!request.is_listing(), "a change is not a listing");
        assert_eq!(request.request(), 2, "the answer must carry this number");
        // The listing stays on screen while the answer is missing.
        assert!(browser.state().listing().is_some());
        assert!(browser.is_waiting());
    }

    #[test]
    fn names_that_would_mean_another_path_are_refused_before_anything_is_sent() {
        let mut browser = listed_browser();
        assert_eq!(browser.create_directory(""), Err(NameProblem::Empty));
        assert_eq!(browser.create_directory("."), Err(NameProblem::Navigation));
        assert_eq!(browser.create_directory(".."), Err(NameProblem::Navigation));
        assert_eq!(browser.create_directory("a/b"), Err(NameProblem::Separator));
        assert_eq!(
            browser.create_directory("docs"),
            Err(NameProblem::Taken),
            "a name that is already in the listing is a conflict, not a request"
        );
        assert_eq!(
            browser.delete("not-listed"),
            Err(NameProblem::Missing),
            "only an entry the server reported can be deleted"
        );
        assert_eq!(
            browser.rename("notes.txt", "notes.txt"),
            Err(NameProblem::Unchanged)
        );
        // None of the refused names took a request number, so the next real request
        // is still the one after the listing.
        let sent = browser.create_directory("logs").expect("a free name");
        assert_eq!(sent.request(), 2);
        assert!(browser.last_outcome().is_none(), "nothing was changed");
    }

    #[test]
    fn nothing_can_be_created_before_the_directory_is_known() {
        let mut browser = browser();
        assert_eq!(
            browser.create_directory("logs"),
            Err(NameProblem::NoDirectory)
        );
        assert_eq!(browser.delete("logs"), Err(NameProblem::NoDirectory));
    }

    #[test]
    fn a_rename_names_both_paths_and_a_delete_carries_the_kind() {
        let mut browser = listed_browser();
        assert_eq!(
            browser
                .rename("notes.txt", "说明.txt")
                .expect("a free name"),
            BrowseRequest::Rename {
                request: 2,
                from: "/home/deploy/notes.txt".to_owned(),
                to: "/home/deploy/说明.txt".to_owned(),
            }
        );
        browser.apply(BrowseResponse::Changed {
            request: 2,
            outcome: MutationOutcome::new(
                MutationAction::Rename {
                    from: "/home/deploy/notes.txt".to_owned(),
                    to: "/home/deploy/说明.txt".to_owned(),
                },
                vec![MutationStep::done(
                    "/home/deploy/说明.txt",
                    Some(FileKind::File),
                )],
            ),
        });
        assert_eq!(
            browser.delete("docs").expect("a listed directory"),
            BrowseRequest::Delete {
                request: 3,
                path: "/home/deploy/docs".to_owned(),
                kind: FileKind::Directory,
            }
        );
    }

    #[test]
    fn a_recursive_delete_reports_every_path_and_every_refusal() {
        let action = MutationAction::Delete {
            path: "/home/deploy/docs".to_owned(),
            kind: FileKind::Directory,
        };
        assert!(action.is_recursive());
        assert_eq!(action.target(), "/home/deploy/docs");

        let outcome = MutationOutcome::new(
            action.clone(),
            vec![
                MutationStep::done("/home/deploy/docs/a.txt", Some(FileKind::File)),
                MutationStep::failed(
                    "/home/deploy/docs/locked.txt",
                    Some(FileKind::File),
                    BrowseFailure::PermissionDenied {
                        path: "/home/deploy/docs/locked.txt".to_owned(),
                    },
                ),
                MutationStep::failed(
                    "/home/deploy/docs",
                    Some(FileKind::Directory),
                    BrowseFailure::Refused {
                        reason: "Directory not empty".to_owned(),
                    },
                ),
            ],
        );
        assert!(!outcome.succeeded(), "a partial failure is not a success");
        assert_eq!(outcome.changed(), 1);
        assert_eq!(outcome.refused_paths(), 2);
        let failed: Vec<&str> = outcome.failures().map(MutationStep::path).collect();
        assert_eq!(
            failed,
            vec!["/home/deploy/docs/locked.txt", "/home/deploy/docs"]
        );
        assert!(matches!(
            outcome.first_failure(),
            Some(BrowseFailure::PermissionDenied { .. })
        ));
        let printed = outcome.to_string();
        assert!(
            printed.contains("2 of 3"),
            "the counts are stated: {printed}"
        );
        assert!(
            printed.contains("locked.txt"),
            "the reason names the path it happened at: {printed}"
        );

        let complete = MutationOutcome::new(
            action,
            vec![MutationStep::done(
                "/home/deploy/docs",
                Some(FileKind::Directory),
            )],
        );
        assert!(complete.succeeded());
        assert!(complete.to_string().contains("/home/deploy/docs"));
    }

    #[test]
    fn a_change_that_reached_nothing_is_not_a_success() {
        let outcome = MutationOutcome::new(
            MutationAction::CreateDirectory {
                path: "/home/deploy/logs".to_owned(),
            },
            Vec::new(),
        );
        assert!(!outcome.succeeded(), "no step means nothing happened");
        assert_eq!(outcome.changed(), 0);
        assert!(outcome.first_failure().is_none());

        let refused = MutationOutcome::refused(
            MutationAction::CreateDirectory {
                path: "/home/deploy/logs".to_owned(),
            },
            BrowseFailure::AlreadyExists {
                path: "/home/deploy/logs".to_owned(),
            },
        );
        assert!(!refused.succeeded());
        assert_eq!(
            refused.steps().len(),
            1,
            "the target is named in the report"
        );
        assert_eq!(refused.steps()[0].path(), "/home/deploy/logs");
    }

    #[test]
    fn a_late_change_answer_never_replaces_a_newer_listing() {
        let mut browser = listed_browser();
        browser.delete("docs").expect("a listed directory");
        // The user leaves the directory before the answer arrives.
        browser.begin_list("/home");

        assert!(
            !browser.apply(BrowseResponse::Changed {
                request: 2,
                outcome: MutationOutcome::new(
                    MutationAction::Delete {
                        path: "/home/deploy/docs".to_owned(),
                        kind: FileKind::Directory,
                    },
                    vec![MutationStep::done(
                        "/home/deploy/docs",
                        Some(FileKind::Directory)
                    )],
                ),
            }),
            "the answer belongs to a request the browser already replaced"
        );
        assert!(browser.last_outcome().is_none());
        assert!(browser.state().is_loading());
        assert_eq!(browser.path(), Some("/home"));
    }

    #[test]
    fn a_change_answer_belongs_to_the_request_it_answers_and_stays_until_the_next_one() {
        let mut browser = listed_browser();
        browser.delete("notes.txt").expect("a listed file");
        let outcome = MutationOutcome::new(
            MutationAction::Delete {
                path: "/home/deploy/notes.txt".to_owned(),
                kind: FileKind::File,
            },
            vec![MutationStep::done(
                "/home/deploy/notes.txt",
                Some(FileKind::File),
            )],
        );
        assert!(browser.apply(BrowseResponse::Changed {
            request: 2,
            outcome: outcome.clone(),
        }));
        assert_eq!(browser.last_outcome(), Some(&outcome));
        assert!(!browser.is_waiting());
        // The listing on screen is still the one from before the change; the refresh
        // the view asks for next is what replaces it.
        assert_eq!(
            browser.state().listing().map(DirectoryListing::path),
            Some("/home/deploy")
        );

        browser.create_directory("logs").expect("a free name");
        assert!(
            browser.last_outcome().is_none(),
            "a new attempt replaces the report of the previous one"
        );
    }

    #[test]
    fn refreshing_asks_again_for_the_directory_on_screen() {
        let mut browser = browser();
        browser.begin_home();
        assert_eq!(browser.refresh(), BrowseRequest::Home { request: 2 });
        browser.apply(listed(2, "/home/deploy", Vec::new()));
        assert_eq!(
            browser.refresh(),
            BrowseRequest::List {
                request: 3,
                path: "/home/deploy".to_owned(),
            }
        );
        assert!(browser.is_waiting());
    }
}
