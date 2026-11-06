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
            | Self::PermissionDenied { path } => Some(path),
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
            Self::PermissionDenied { path } => {
                write!(formatter, "the server refused to read {path}")
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
}

impl BrowseRequest {
    /// The number the answer must carry.
    #[must_use]
    pub fn request(&self) -> u64 {
        match self {
            Self::Home { request } | Self::List { request, .. } => *request,
        }
    }

    /// The path that was asked for, when one was named.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Home { .. } => None,
            Self::List { path, .. } => Some(path),
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
}

impl BrowseResponse {
    /// The request this answers.
    #[must_use]
    pub fn request(&self) -> u64 {
        match self {
            Self::Listed { request, .. } | Self::Failed { request, .. } => *request,
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

    /// Applies an answer, returning whether it belonged to the open request.
    ///
    /// An answer for another request is dropped: it belongs to a directory the user
    /// has already left, or to a request this browser never made.
    pub fn apply(&mut self, response: BrowseResponse) -> bool {
        if self.outstanding != Some(response.request()) {
            return false;
        }
        self.outstanding = None;
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
        }
        true
    }

    /// Numbers the next request and marks the browser as waiting for it.
    fn begin(&mut self) -> u64 {
        let request = self.next_request;
        self.next_request += 1;
        self.outstanding = Some(request);
        self.state = BrowseState::Loading;
        request
    }
}

/// Whether a string can be one entry name.
///
/// A name with a separator would turn "open this entry" into "open another path",
/// and "." or ".." are the browser's navigation rather than entries of a listing.
#[must_use]
pub fn is_single_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
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
