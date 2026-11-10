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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shellune_core::{
    BrowseFailure, BrowseRequest, BrowseResponse, FileBrowser, FileKind, MutationAction,
    MutationOutcome, MutationStep, NameProblem, SessionId, TransferProtocol, join_path,
    normalize_path, parent_path,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
            },
        );
    }

    /// Replaces the local channel, for a test that scripts this computer.
    ///
    /// The remote channel is left as it was: a script for one source must not become
    /// the other source's answers.
    #[cfg(test)]
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
            let request = remote.begin_home();
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
    pub fn up_in(&mut self, session: SessionId, source: FileSource) -> bool {
        self.ask_in(session, source, |browser| browser.up())
    }

    /// Asks again for the remote directory shown.
    pub fn refresh(&mut self, session: SessionId) -> bool {
        self.refresh_in(session, FileSource::Remote)
    }

    /// Asks again for one source. The next listing, not the previous one, is truth.
    pub fn refresh_in(&mut self, session: SessionId, source: FileSource) -> bool {
        self.ask_in(session, source, |browser| Some(browser.refresh()))
    }

    /// Opens one remote entry.
    pub fn enter(&mut self, session: SessionId, name: &str) -> bool {
        self.enter_in(session, FileSource::Remote, name)
    }

    /// Opens one entry of one source.
    pub fn enter_in(&mut self, session: SessionId, source: FileSource, name: &str) -> bool {
        self.ask_in(session, source, |browser| browser.open(name))
    }

    /// Asks one source to list an absolute path it already speaks.
    ///
    /// Remote paths are POSIX. Local paths are the protocol form [`local_protocol_to_os`]
    /// reverses, so a breadcrumb cannot point the remote browser at a local disk.
    pub fn list_at(&mut self, session: SessionId, source: FileSource, path: &str) -> bool {
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
    pub fn delete(&mut self, session: SessionId, name: &str) -> Result<(), NameProblem> {
        self.delete_in(session, FileSource::Remote, name)
    }

    /// Asks one source to delete one entry.
    pub fn delete_in(
        &mut self,
        session: SessionId,
        source: FileSource,
        name: &str,
    ) -> Result<(), NameProblem> {
        self.mutate_in(session, source, |browser| browser.delete(name))
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
        let size = (kind == FileKind::File)
            .then(|| item.metadata().ok().map(|meta| meta.len()))
            .flatten();
        if let Some(entry) = shellune_core::DirEntry::new(name.as_ref(), kind, size) {
            entries.push(entry);
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
    /// One entry is being deleted, with everything inside it.
    Delete {
        /// The session whose directory holds the entry.
        session: SessionId,
        /// Which source holds the entry.
        source: FileSource,
        /// The entry as the listing named it.
        name: String,
        /// What the entry is, as the listing reported it.
        kind: FileKind,
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
            | Self::Download { session, .. } => *session,
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
            | Self::Download { source, .. } => *source,
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
            | Self::Download { text, .. } => Some(text),
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
            Self::Upload { text, problem, .. } | Self::Download { text, problem, .. } => {
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
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => {}
        }
    }

    /// The remote path of a transfer question, when it has one.
    #[must_use]
    pub fn remote(&self) -> Option<&str> {
        match self {
            Self::Upload { directory, .. } => Some(directory),
            Self::Download { remote, .. } => Some(remote),
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => None,
        }
    }

    /// Which protocol this question would copy with, when it is a transfer question.
    #[must_use]
    pub fn protocol(&self) -> Option<TransferProtocol> {
        match self {
            Self::Upload { protocol, .. } | Self::Download { protocol, .. } => Some(*protocol),
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => None,
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
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => {}
        }
    }

    /// Whether this question would copy a directory with its contents.
    #[must_use]
    pub fn recursive(&self) -> bool {
        match self {
            Self::Upload { recursive, .. } | Self::Download { recursive, .. } => *recursive,
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => false,
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
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => {}
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
            Self::Upload { .. } | Self::Download { .. } => {}
        }
    }

    /// States why a transfer could not start, keeping what the user typed.
    pub fn fail_path(&mut self, failure: PathProblem) {
        match self {
            Self::Upload { problem, .. } | Self::Download { problem, .. } => {
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
            Self::Upload { .. } | Self::Download { .. } => None,
        }
    }

    /// Why a transfer could not start, when it could not.
    #[must_use]
    pub fn path_problem(&self) -> Option<&PathProblem> {
        match self {
            Self::Upload { problem, .. } | Self::Download { problem, .. } => problem.as_ref(),
            Self::NewDirectory { .. } | Self::Rename { .. } | Self::Delete { .. } => None,
        }
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use shellune_core::DirEntry;

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
            name: "docs".to_owned(),
            kind: FileKind::Directory,
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
}
