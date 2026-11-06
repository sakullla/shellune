//! The file view of a remote session: what is open, where it is and what arrived (R9).
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

use std::collections::HashMap;

use shellune_core::{
    BrowseFailure, BrowseRequest, BrowseResponse, FileBrowser, FileKind, SessionId,
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

    /// Forgets a session entirely, which is what closing it does.
    pub fn detach(&mut self, session: SessionId) {
        self.sessions.remove(&session);
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

    /// Applies every answer that arrived, returning whether anything changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        for files in self.sessions.values_mut() {
            let Some(browser) = files.browser.as_mut() else {
                continue;
            };
            for response in files.channel.drain() {
                // An answer that does not belong to the request this browser is
                // waiting for is dropped by the browser itself.
                if browser.apply(response) {
                    changed = true;
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

    /// A channel that records requests and answers the ones a test chooses.
    #[derive(Default)]
    struct FakeChannel {
        sent: Vec<BrowseRequest>,
        answers: Vec<BrowseResponse>,
        accepting: bool,
    }

    impl FakeChannel {
        fn accepting() -> Self {
            Self {
                sent: Vec::new(),
                answers: Vec::new(),
                accepting: true,
            }
        }

        fn reply(&mut self, request: u64, path: &str, names: &[(&str, FileKind)]) {
            let entries = names
                .iter()
                .filter_map(|(name, kind)| DirEntry::new(*name, *kind, None))
                .collect();
            self.answers.push(BrowseResponse::Listed {
                request,
                path: path.to_owned(),
                entries,
            });
        }
    }

    impl BrowseChannel for FakeChannel {
        fn send(&mut self, request: BrowseRequest) -> bool {
            if !self.accepting {
                return false;
            }
            self.sent.push(request);
            true
        }

        fn drain(&mut self) -> Vec<BrowseResponse> {
            std::mem::take(&mut self.answers)
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
        let mut channel = FakeChannel::accepting();
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
        let mut channel = FakeChannel::accepting();
        channel.accepting = false;
        let mut views = views(session, channel);
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
        let mut first_channel = FakeChannel::accepting();
        first_channel.reply(1, "/home/deploy/one", &[("one.txt", FileKind::File)]);
        let mut second_channel = FakeChannel::accepting();
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
        let mut channel = FakeChannel::accepting();
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
}
