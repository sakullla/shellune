//! The file view of a remote session: what is open, where it is and what arrived
//! (R9), and the changes it asks for (R11).
//!
//! The state machine that decides what a directory listing may say lives in
//! shellune-core, because it has to be testable without a window. What lives here is
//! the wiring: one open view per session, the connection it asks, and the rule that a
//! failure of the file view never becomes a failure of the terminal.
//!
//! Three things are kept per session and nothing is shared between sessions:
//!
//! * the link to that session's own connection, so browsing never re-authenticates;
//! * the browser with its path, its request number and its listing;
//! * the host label, so the view can always say which machine a path belongs to.
//!
//! The view asks for entries only after a real answer: pump() applies what came back
//! and reports whether anything changed, and a request that could not even be
//! delivered is turned into a connection failure for that one view.
//!
//! A change is asked for the same way, but it is answered differently: the answer is a
//! report of every path the server reached, and the directory is listed again after it.
//! The report says what happened; only the next listing says what the directory holds
//! now, which is why the two are never mixed into one statement.

use std::collections::HashMap;

use shellune_core::{
    BrowseFailure, BrowseRequest, BrowseResponse, FileBrowser, FileKind, MutationOutcome,
    NameProblem, SessionId,
};
use shellune_ssh::FileLink;

use crate::app::copy::Strings;

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

/// One browsable session: its connection, its view and the name of the host.
struct SessionFiles {
    channel: Box<dyn BrowseChannel>,
    /// None until the user opens the file view for this session.
    browser: Option<FileBrowser>,
    host: String,
    target: String,
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
    /// without naming the machine it is on.
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
                channel,
                browser: None,
                host: host.into(),
                target: target.into(),
            },
        );
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
            .is_some_and(|files| files.browser.is_some())
    }

    /// Whether any file view is open, which is when polling is worth its timer.
    #[must_use]
    pub fn any_open(&self) -> bool {
        self.sessions.values().any(|files| files.browser.is_some())
    }

    /// The view of one session, when it is open.
    #[must_use]
    pub fn browser(&self, session: SessionId) -> Option<&FileBrowser> {
        self.sessions.get(&session)?.browser.as_ref()
    }

    /// What the last change in this view really did, when one was attempted.
    #[must_use]
    pub fn outcome(&self, session: SessionId) -> Option<&MutationOutcome> {
        self.browser(session)?.last_outcome()
    }

    /// Opens the file view of a session at the login directory.
    ///
    /// Opening is the only place that starts a browser, so a session that was never
    /// opened has no path and no listing to show for itself.
    pub fn open(&mut self, session: SessionId) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if files.browser.is_none() {
            let mut browser = FileBrowser::new(session, files.host.clone(), files.target.clone());
            let request = browser.begin_home();
            files.browser = Some(browser);
            let _ = Self::ask(files, request);
        }
        self.is_open(session)
    }

    /// Closes the file view of a session, keeping the connection for a later open.
    pub fn close(&mut self, session: SessionId) {
        if let Some(files) = self.sessions.get_mut(&session) {
            files.browser = None;
        }
    }

    /// Forgets a session entirely, which is what a closed connection does.
    ///
    /// The connection ends with the session, so the view of it ends too: a listing
    /// that nothing backs would keep showing files the user cannot reach, and it would
    /// keep the poll timer alive for a server that is no longer there.
    pub fn detach(&mut self, session: SessionId) -> bool {
        let had_view = self.is_open(session);
        self.sessions.remove(&session);
        had_view
    }

    /// Asks for the parent of the directory shown.
    pub fn up(&mut self, session: SessionId) -> bool {
        self.ask_from(session, |browser| browser.up())
    }

    /// Asks again for the directory shown.
    pub fn refresh(&mut self, session: SessionId) -> bool {
        self.ask_from(session, |browser| Some(browser.refresh()))
    }

    /// Opens one entry of the listing on screen.
    pub fn enter(&mut self, session: SessionId, name: &str) -> bool {
        self.ask_from(session, |browser| browser.open(name))
    }

    /// Asks for a new directory called name inside the directory on screen.
    ///
    /// A name the browser refuses produces the reason and sends nothing: an invalid
    /// name or a collision is a fact about the listing on screen, so it is answered
    /// before the server is involved.
    pub fn create_directory(&mut self, session: SessionId, name: &str) -> Result<(), NameProblem> {
        self.mutate(session, |browser| browser.create_directory(name))
    }

    /// Asks to rename one entry of the directory on screen.
    pub fn rename(&mut self, session: SessionId, from: &str, to: &str) -> Result<(), NameProblem> {
        self.mutate(session, |browser| browser.rename(from, to))
    }

    /// Asks to delete one entry of the directory on screen.
    pub fn delete(&mut self, session: SessionId, name: &str) -> Result<(), NameProblem> {
        self.mutate(session, |browser| browser.delete(name))
    }

    /// Runs one change of the browser and delivers it.
    fn mutate(
        &mut self,
        session: SessionId,
        ask: impl FnOnce(&mut FileBrowser) -> Result<BrowseRequest, NameProblem>,
    ) -> Result<(), NameProblem> {
        let Some(files) = self.sessions.get_mut(&session) else {
            return Err(NameProblem::NoDirectory);
        };
        let Some(browser) = files.browser.as_mut() else {
            return Err(NameProblem::NoDirectory);
        };
        let request = ask(browser)?;
        Self::ask(files, request);
        Ok(())
    }

    /// Applies every answer that arrived, returning whether anything changed.
    ///
    /// A change is answered only by the server's own next listing: once the report is
    /// in, the directory is listed again, so what stays on screen is what the server
    /// really holds now and not what the change hoped for.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        for files in self.sessions.values_mut() {
            let responses = files.channel.drain();
            if files.browser.is_none() {
                continue;
            }
            for response in responses {
                let is_change = response.outcome().is_some();
                let applied = {
                    let Some(browser) = files.browser.as_mut() else {
                        break;
                    };
                    // An answer that does not belong to the request this browser is
                    // waiting for is dropped by the browser itself.
                    browser.apply(response)
                };
                if !applied {
                    continue;
                }
                changed = true;
                if is_change {
                    let refreshed = {
                        let Some(browser) = files.browser.as_mut() else {
                            break;
                        };
                        browser.refresh()
                    };
                    Self::ask(files, refreshed);
                }
            }
        }
        changed
    }

    /// Runs one request of the browser and delivers it.
    fn ask_from(
        &mut self,
        session: SessionId,
        ask: impl FnOnce(&mut FileBrowser) -> Option<BrowseRequest>,
    ) -> bool {
        let Some(files) = self.sessions.get_mut(&session) else {
            return false;
        };
        if files.browser.is_none() {
            return false;
        }
        let request = files.browser.as_mut().and_then(ask);
        let Some(request) = request else {
            return false;
        };
        Self::ask(files, request)
    }

    /// Delivers one request, reporting a connection that cannot take it.
    ///
    /// A request that never left the process is answered here, as the failure it is:
    /// the view must not sit in "loading" for a connection that is already gone.
    fn ask(files: &mut SessionFiles, request: BrowseRequest) -> bool {
        if files.channel.send(request.clone()) {
            return true;
        }
        if let Some(browser) = files.browser.as_mut() {
            browser.apply(BrowseResponse::Failed {
                request: request.request(),
                failure: BrowseFailure::ConnectionLost {
                    reason: "the SSH connection is no longer open".to_owned(),
                },
            });
        }
        false
    }
}
/// The question the file view is asking before anything is sent (R11).
///
/// A change to a server is asked for twice: once in the dialog, where the user sees
/// the exact target, the kind and - for a directory - that its contents go too, and
/// once when the answer arrives. Nothing is sent while this exists, so cancelling is
/// never a request the server has to ignore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesDialog {
    /// A new directory is named but not created.
    NewDirectory {
        /// The session whose directory it would go into.
        session: SessionId,
        /// What the user typed.
        text: String,
        /// Why the last attempt to send it was refused, when it was.
        problem: Option<NameProblem>,
    },
    /// One entry is being renamed.
    Rename {
        /// The session whose directory holds the entry.
        session: SessionId,
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
        /// The entry as the listing named it.
        name: String,
        /// What the entry is, as the listing reported it.
        kind: FileKind,
    },
}

impl FilesDialog {
    /// The session this question belongs to.
    #[must_use]
    pub fn session(&self) -> SessionId {
        match self {
            Self::NewDirectory { session, .. }
            | Self::Rename { session, .. }
            | Self::Delete { session, .. } => *session,
        }
    }

    /// The text the user is typing, when the question has a field.
    #[cfg(test)]
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::NewDirectory { text, .. } | Self::Rename { text, .. } => Some(text),
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
            Self::Delete { .. } => {}
        }
    }

    /// States why the text cannot be sent, keeping the text in front of the user.
    pub fn fail(&mut self, failure: NameProblem) {
        match self {
            Self::NewDirectory { problem, .. } | Self::Rename { problem, .. } => {
                *problem = Some(failure);
            }
            // A deletion has no text to correct; a refusal about it is answered by the
            // browser instead of by this question.
            Self::Delete { .. } => {}
        }
    }

    /// Why the text cannot be sent, when it cannot.
    #[must_use]
    pub fn problem(&self) -> Option<&NameProblem> {
        match self {
            Self::NewDirectory { problem, .. } | Self::Rename { problem, .. } => problem.as_ref(),
            Self::Delete { .. } => None,
        }
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
}
