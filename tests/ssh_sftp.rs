//! Browsing a server's directories over the shell's own connection (R9, R12).
//!
//! Every test here talks to the disposable OpenSSH server from
//! scripts/ssh-test-server.sh through a real connection: the terminal logs in with a
//! password, and the directory browser uses that same connection. That is the point
//! of the file, so nothing here stands in for the transport:
//!
//! * the names that are listed are made on the server by the test, including a
//!   directory whose name has Chinese characters and spaces, and what the browser
//!   reports is compared with what ls prints on the server for the same directory;
//! * a directory the account may not read, a path that does not exist and a file
//!   that is not a directory are real conditions of that server, not injected
//!   failures;
//! * a second server that is configured without an SFTP subsystem shows what the
//!   product does when the server has no SFTP at all;
//! * after each case the terminal is used again, and the server's log is read, so
//!   "the terminal still works" and "no second authentication happened" are
//!   observations rather than assumptions.
//!
//! Anywhere this host cannot start such a server the test prints why and returns: a
//! run without a controllable server never claims SFTP support was verified.

use std::time::{Duration, Instant};

use shellune_core::{
    AuthMethod, BrowseFailure, BrowseRequest, BrowseResponse, DirEntry, FileKind, MutationOutcome,
    MutationStep,
};
use shellune_ssh::testserver::Harness;
use shellune_ssh::{ConnectRequest, FileLink, KnownHostKey, PtySize, RemoteEvent, RemoteSession};

/// How long any single wait may take.
const PATIENCE: Duration = Duration::from_secs(45);

/// The directory the fixtures live in, relative to the account's home.
const CHINESE_DIR: &str = "文档 与 空格";

/// The server the ordinary cases share, started once.
fn shared() -> &'static Option<Harness> {
    static SERVER: std::sync::OnceLock<Option<Harness>> = std::sync::OnceLock::new();
    SERVER.get_or_init(|| {
        if !Harness::is_available() {
            return None;
        }
        let server = Harness::start("sftp", Harness::free_port()).ok()?;
        prepare(&server).ok()?;
        Some(server)
    })
}

/// What to do when this host cannot run the server.
fn skip(what: &str) -> bool {
    if shared().is_some() {
        return false;
    }
    eprintln!(
        "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so no SFTP \
         behaviour is verified here and none is claimed. On Windows the tests start one \
         through WSL by themselves; elsewhere scripts/ssh-test-server.sh has to run on a \
         Linux host the tests can reach."
    );
    true
}

/// Creates the fixtures on the server, as the account the tests log in as.
fn prepare(server: &Harness) -> Result<(), String> {
    let home = server.home();
    let script = format!(
        "set -e
mkdir -p '{home}/{CHINESE_DIR}/sub'
mkdir -p '{home}/unreadable'
printf 'shellune fixture\n' > '{home}/{CHINESE_DIR}/readme.txt'
printf 'nested\n' > '{home}/{CHINESE_DIR}/sub/deep.txt'
chmod 000 '{home}/unreadable'
rm -f '{home}/plain.txt'
printf 'plain\n' > '{home}/plain.txt'
ls -A '{home}'"
    );
    server.as_user(&script).map(|_| ())
}

/// The password attempt with the identity the server's own key really has.
fn connect(server: &Harness) -> RemoteSession {
    RemoteSession::connect(ConnectRequest::new(
        "127.0.0.1",
        server.port(),
        server.user(),
        AuthMethod::password(server.password()),
        PtySize::new(100, 30),
        KnownHostKey::Trusted {
            fingerprint: server.host_key_fingerprint(),
        },
    ))
}

/// Connects and waits until the remote shell really runs.
fn live(server: &Harness) -> RemoteSession {
    let mut session = connect(server);
    let deadline = Instant::now() + PATIENCE;
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::Connected => return session,
                RemoteEvent::Failed(error) => panic!("the attempt failed: {error}"),
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "the attempt neither connected nor failed in time"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits for one answer, which must arrive on its own.
fn answer(link: &mut FileLink) -> BrowseResponse {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(response) = link.drain().into_iter().next() {
            return response;
        }
        assert!(Instant::now() < deadline, "the file browser never answered");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Lists one path and returns its entries, or the failure.
fn list(link: &mut FileLink, request: u64, path: &str) -> Result<Vec<DirEntry>, BrowseFailure> {
    assert!(
        link.send(BrowseRequest::List {
            request,
            path: path.to_owned(),
        }),
        "the request reaches the connection"
    );
    match answer(link) {
        BrowseResponse::Listed {
            request: answered,
            entries,
            ..
        } => {
            assert_eq!(answered, request);
            Ok(entries)
        }
        BrowseResponse::Failed {
            request: answered,
            failure,
        } => {
            assert_eq!(answered, request);
            Err(failure)
        }
        BrowseResponse::Changed {
            request: answered, ..
        } => {
            panic!("listing {path} was answered with a change report ({answered})")
        }
    }
}

/// The entries as name/kind pairs, in the order the browser reports them.
fn named(entries: &[DirEntry]) -> Vec<(String, FileKind)> {
    entries
        .iter()
        .map(|entry| (entry.name().to_owned(), entry.kind()))
        .collect()
}

/// Uses the terminal again and returns everything it printed.
fn shell_echo(session: &mut RemoteSession, text: &str) -> String {
    session
        .write(format!("echo {text}\r\n").as_bytes())
        .expect("the shell accepts input");
    let deadline = Instant::now() + PATIENCE;
    let mut output = Vec::new();
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::Data(bytes) => output.extend_from_slice(&bytes),
                RemoteEvent::Failed(error) => panic!("the terminal stopped: {error}"),
                _ => {}
            }
        }
        let printed = String::from_utf8_lossy(&output).into_owned();
        if printed.contains(text) {
            return printed;
        }
        assert!(
            Instant::now() < deadline,
            "the terminal did not answer after browsing; it printed {printed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How many times the server accepted an authentication.
fn accepted_authentications(server: &Harness) -> usize {
    server
        .log()
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("Accepted"))
        .count()
}

/// A server of this test's own, so counting logins in it cannot see another test's.
fn own_server(name: &str) -> Harness {
    let server = Harness::start(name, Harness::free_port())
        .unwrap_or_else(|reason| panic!("the server {name} could not be started: {reason}"));
    prepare(&server).unwrap_or_else(|reason| panic!("the fixtures of {name} failed: {reason}"));
    server
}

#[test]
fn the_home_directory_is_listed_on_the_connection_the_shell_authenticated() {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED listing the home directory: this host cannot start a disposable OpenSSH \
             server, so nothing is verified here and nothing is claimed."
        );
        return;
    }
    // A server of this test's own: the evidence below is that logging in happened once
    // for the terminal and never again for the directory, which only means something if
    // no other test logs into the same server at the same time.
    let server = own_server("sftp-count");
    let server = &server;
    let before = accepted_authentications(server);

    let mut session = live(server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    assert!(
        link.send(BrowseRequest::Home { request: 1 }),
        "the home request reaches the connection"
    );
    let home = match answer(&mut link) {
        BrowseResponse::Listed { path, entries, .. } => {
            assert!(
                path.ends_with(server.user()) || path.starts_with("/home/"),
                "the browser reports the directory the server resolved, got {path}"
            );
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name() == CHINESE_DIR && entry.is_directory()),
                "a directory named with Chinese characters and spaces is listed as one: {:?}",
                named(&entries)
            );
            path
        }
        BrowseResponse::Failed { failure, .. } => panic!("the home directory failed: {failure}"),
        BrowseResponse::Changed { .. } => panic!("the home request was answered with a change"),
    };
    assert!(
        home.starts_with('/'),
        "the path the browser shows is absolute, got {home}"
    );

    // The same directory, listed by the server itself, contains the same names.
    let printed = server
        .as_user(&format!("ls -A '{home}'"))
        .expect("the server lists the directory");
    let mut expected: Vec<&str> = printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    expected.sort_unstable();
    let mut listed: Vec<String> = list(&mut link, 2, &home)
        .expect("the directory is listed again")
        .into_iter()
        .map(|entry| entry.name().to_owned())
        .collect();
    listed.sort();
    assert_eq!(
        listed, expected,
        "the browser reports exactly the entries ls prints for the same directory"
    );

    // Browsing used the connection the shell already had: no second login appears in
    // the server's log, and the shell still answers.
    assert_eq!(
        accepted_authentications(server),
        before + 1,
        "one authentication for the terminal, none for browsing"
    );
    shell_echo(&mut session, "shellune-still-alive");
    session.close();
}

#[test]
fn a_subdirectory_and_the_parent_directory_match_the_server() {
    if skip("navigating directories") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let home = server.home();
    let mut session = live(server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    let chinese = format!("{home}/{CHINESE_DIR}");
    let entries = list(&mut link, 1, &chinese).expect("the directory is readable");
    let kinds = named(&entries);
    assert!(
        kinds.contains(&("sub".to_owned(), FileKind::Directory)),
        "the subdirectory is listed as a directory: {kinds:?}"
    );
    assert!(
        kinds.contains(&("readme.txt".to_owned(), FileKind::File)),
        "the file is listed as a file: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|(name, _)| name == "." || name == ".."),
        "the listing holds entries, not the navigation entries"
    );

    // What the server has inside the directory is what the browser reported.
    let printed = server
        .as_user(&format!("ls -A '{chinese}'"))
        .expect("the server lists the directory");
    let mut expected: Vec<String> = printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    expected.sort();
    let mut listed: Vec<String> = kinds.iter().map(|(name, _)| name.clone()).collect();
    listed.sort();
    assert_eq!(listed, expected);

    let nested = list(&mut link, 2, &format!("{chinese}/sub")).expect("the subdirectory opens");
    assert_eq!(
        named(&nested),
        vec![("deep.txt".to_owned(), FileKind::File)]
    );

    // The parent of the Chinese directory is the home directory the server has.
    let parent = list(&mut link, 3, &home).expect("the parent is listed");
    assert!(
        parent.iter().any(|entry| entry.name() == CHINESE_DIR),
        "going up reaches the directory that holds it: {:?}",
        named(&parent)
    );
    session.close();
}

#[test]
fn a_directory_the_account_may_not_read_reports_the_refusal() {
    if skip("an unreadable directory") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let home = server.home();
    let mut session = live(server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    let blocked = format!("{home}/unreadable");
    let failure = list(&mut link, 1, &blocked).expect_err("the directory cannot be read");
    assert!(
        matches!(failure, BrowseFailure::PermissionDenied { .. }),
        "the refusal is reported as a refused directory, got {failure:?}"
    );
    assert_eq!(failure.path(), Some(blocked.as_str()));
    assert!(
        !failure.is_connection_lost(),
        "a refused directory says nothing about the connection"
    );

    // The same connection still lists a directory it may read, and the shell is fine.
    let entries = list(&mut link, 2, &home).expect("the home directory is still readable");
    assert!(entries.iter().any(|entry| entry.name() == CHINESE_DIR));
    shell_echo(&mut session, "shellune-after-refusal");
    session.close();
}

#[test]
fn a_missing_path_and_a_file_are_told_apart() {
    if skip("a missing path") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let home = server.home();
    let mut session = live(server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    let missing = format!("{home}/no-such-directory");
    let failure = list(&mut link, 1, &missing).expect_err("the path does not exist");
    assert!(
        matches!(failure, BrowseFailure::NotFound { .. }),
        "a missing path is its own failure, got {failure:?}"
    );
    assert_eq!(failure.path(), Some(missing.as_str()));

    let file = format!("{home}/plain.txt");
    let failure = list(&mut link, 2, &file).expect_err("a file has no listing");
    assert!(
        matches!(failure, BrowseFailure::NotADirectory { .. }),
        "a file is not a directory, got {failure:?}"
    );

    shell_echo(&mut session, "shellune-after-missing");
    session.close();
}

#[test]
fn a_server_without_sftp_says_so_and_keeps_the_terminal() {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED a server without SFTP: this host cannot start a disposable OpenSSH server, \
             so nothing about a missing SFTP subsystem is verified here."
        );
        return;
    }
    let server = match Harness::start_without_sftp("sftp-none", Harness::free_port()) {
        Ok(server) => server,
        Err(reason) => panic!("the server without SFTP could not be started: {reason}"),
    };
    let mut session = live(&server);
    let mut link = session.take_file_link().expect("the session is browsable");

    assert!(
        link.send(BrowseRequest::Home { request: 1 }),
        "the request reaches the connection"
    );
    match answer(&mut link) {
        BrowseResponse::Failed { failure, .. } => assert!(
            matches!(failure, BrowseFailure::Unavailable { .. }),
            "a server without SFTP is reported as exactly that, got {failure:?}"
        ),
        BrowseResponse::Listed { entries, .. } => panic!(
            "a server without an SFTP subsystem cannot list a directory, it answered with {} entries",
            entries.len()
        ),
        BrowseResponse::Changed { .. } => panic!("a listing request was answered with a change"),
    }

    // The terminal was never involved: it still runs on the same connection.
    let printed = shell_echo(&mut session, "shellune-no-sftp");
    assert!(
        printed.contains("shellune-no-sftp"),
        "the shell answered after the SFTP refusal"
    );
    session.close();
}

/// Sends one change and returns the report the server's answer produced.
///
/// A change is always answered with a report, even when the server refused it: the
/// failures are inside the outcome, which is what lets the view state what really
/// happened to each path.
fn change(link: &mut FileLink, request: BrowseRequest) -> MutationOutcome {
    assert!(link.send(request), "the request reaches the connection");
    match answer(link) {
        BrowseResponse::Changed { outcome, .. } => outcome,
        BrowseResponse::Failed { failure, .. } => {
            panic!("the change was refused outright instead of reported: {failure}")
        }
        BrowseResponse::Listed { .. } => panic!("a change is not answered with a listing"),
    }
}

/// Whether the server really has this path.
fn server_has(server: &Harness, path: &str) -> bool {
    server
        .as_user(&format!("test -e '{path}' && echo yes"))
        .unwrap_or_default()
        .contains("yes")
}

/// The names the server itself lists in one directory.
fn server_names(server: &Harness, path: &str) -> Vec<String> {
    let printed = server
        .as_user(&format!("ls -A '{path}'"))
        .expect("the server lists the directory");
    let mut names: Vec<String> = printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

/// The names the browser lists in one directory.
fn listed_names(link: &mut FileLink, request: u64, path: &str) -> Vec<String> {
    let mut names: Vec<String> = list(link, request, path)
        .expect("the directory is listed")
        .into_iter()
        .map(|entry| entry.name().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_directory_is_created_renamed_and_deleted_on_the_server() {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED changing a directory: this host cannot start a disposable OpenSSH \
             server, so nothing is verified here and nothing is claimed."
        );
        return;
    }
    let server = own_server("sftp-mutate");
    let home = server.home();
    let mut session = live(&server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    // Creating a directory with Chinese characters and a space.
    let created = format!("{home}/新建 目录");
    let outcome = change(
        &mut link,
        BrowseRequest::CreateDirectory {
            request: 1,
            directory: home.clone(),
            name: "新建 目录".to_owned(),
        },
    );
    assert!(outcome.succeeded(), "the directory was created: {outcome}");
    assert_eq!(outcome.steps()[0].path(), created);
    assert!(
        server_has(&server, &created),
        "the directory really exists on the server"
    );
    let expected = server_names(&server, &home);
    assert_eq!(
        listed_names(&mut link, 2, &home),
        expected,
        "the listing after the change is the one the server reports"
    );
    assert!(
        expected.contains(&"新建 目录".to_owned()),
        "the new name is in the server's own listing: {expected:?}"
    );

    // The same name again is a conflict, and the server is not asked twice.
    let outcome = change(
        &mut link,
        BrowseRequest::CreateDirectory {
            request: 3,
            directory: home.clone(),
            name: "新建 目录".to_owned(),
        },
    );
    assert!(
        !outcome.succeeded(),
        "an existing name is not a successful creation: {outcome}"
    );
    assert!(
        matches!(
            outcome.first_failure(),
            Some(BrowseFailure::AlreadyExists { .. })
        ),
        "the conflict is named as one: {outcome}"
    );

    // Renaming it, in its own directory.
    let renamed = format!("{home}/改名后");
    let outcome = change(
        &mut link,
        BrowseRequest::Rename {
            request: 4,
            from: created.clone(),
            to: renamed.clone(),
        },
    );
    assert!(outcome.succeeded(), "the rename happened: {outcome}");
    assert!(!server_has(&server, &created), "the old name is gone");
    assert!(server_has(&server, &renamed), "the new name exists");
    assert_eq!(
        listed_names(&mut link, 5, &home),
        server_names(&server, &home),
        "the list after the rename is the server's list"
    );

    // A rename onto a name that is taken is refused, and nothing is replaced.
    let outcome = change(
        &mut link,
        BrowseRequest::Rename {
            request: 6,
            from: renamed.clone(),
            to: format!("{home}/{CHINESE_DIR}"),
        },
    );
    assert!(
        matches!(
            outcome.first_failure(),
            Some(BrowseFailure::AlreadyExists { .. })
        ),
        "a taken name is a conflict: {outcome}"
    );
    assert!(
        server_has(&server, &renamed) && server_has(&server, &format!("{home}/{CHINESE_DIR}")),
        "neither entry was touched by the refused rename"
    );

    // Deleting the empty directory, which really removes it.
    let outcome = change(
        &mut link,
        BrowseRequest::Delete {
            request: 7,
            path: renamed.clone(),
            kind: FileKind::Directory,
        },
    );
    assert!(outcome.succeeded(), "the directory was deleted: {outcome}");
    assert!(!server_has(&server, &renamed), "it is gone from the server");
    assert_eq!(
        listed_names(&mut link, 8, &home),
        server_names(&server, &home),
        "the list after the delete is the server's list"
    );

    shell_echo(&mut session, "shellune-after-mutations");
    session.close();
}

#[test]
fn a_recursive_delete_removes_the_contents_and_an_unwritable_one_is_reported_item_by_item() {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED a recursive delete: this host cannot start a disposable OpenSSH \
             server, so nothing is verified here and nothing is claimed."
        );
        return;
    }
    let server = own_server("sftp-delete");
    let home = server.home();
    let tree = format!("{home}/tree");
    server
        .as_user(&format!(
            "set -e
mkdir -p '{tree}/sub'
printf 'a\n' > '{tree}/a.txt'
printf 'b\n' > '{tree}/sub/b.txt'
mkdir -p '{home}/locked'
printf 'k\n' > '{home}/locked/keep.txt'
chmod 500 '{home}/locked'"
        ))
        .expect("the fixtures are created");
    let mut session = live(&server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    let outcome = change(
        &mut link,
        BrowseRequest::Delete {
            request: 1,
            path: tree.clone(),
            kind: FileKind::Directory,
        },
    );
    assert!(outcome.succeeded(), "the whole tree was deleted: {outcome}");
    let removed: Vec<&str> = outcome.steps().iter().map(MutationStep::path).collect();
    assert!(
        removed.contains(&format!("{tree}/a.txt").as_str())
            && removed.contains(&format!("{tree}/sub/b.txt").as_str()),
        "every item the change reached is reported: {removed:?}"
    );
    assert_eq!(
        removed.last().copied(),
        Some(tree.as_str()),
        "the directory itself is removed last, after its contents"
    );
    assert!(!server_has(&server, &tree), "the tree is really gone");
    assert_eq!(
        listed_names(&mut link, 2, &home),
        server_names(&server, &home),
        "the list matches the server after the recursive delete"
    );

    // A directory the account may read but not write: the items inside cannot be
    // removed, and the report says which ones and why.
    let locked = format!("{home}/locked");
    let outcome = change(
        &mut link,
        BrowseRequest::Delete {
            request: 3,
            path: locked.clone(),
            kind: FileKind::Directory,
        },
    );
    assert!(
        !outcome.succeeded(),
        "a partial delete is not a success: {outcome}"
    );
    let failures: Vec<(&str, &BrowseFailure)> = outcome
        .failures()
        .filter_map(|step| step.failure().map(|failure| (step.path(), failure)))
        .collect();
    assert!(
        failures
            .iter()
            .any(|(path, failure)| *path == format!("{locked}/keep.txt")
                && matches!(failure, BrowseFailure::PermissionDenied { .. })),
        "the refused file is reported with its reason: {failures:?}"
    );
    assert!(
        server_has(&server, &format!("{locked}/keep.txt")),
        "the file the server refused to remove is still there"
    );

    // A directory the account may not write is refused the same way for a creation and
    // for a rename, and neither refusal changes anything.
    let outcome = change(
        &mut link,
        BrowseRequest::CreateDirectory {
            request: 5,
            directory: locked.clone(),
            name: "new".to_owned(),
        },
    );
    assert!(
        matches!(
            outcome.first_failure(),
            Some(BrowseFailure::PermissionDenied { .. })
        ),
        "a creation without write permission is refused with the reason: {outcome}"
    );
    assert!(!server_has(&server, &format!("{locked}/new")));

    let outcome = change(
        &mut link,
        BrowseRequest::Rename {
            request: 6,
            from: format!("{locked}/keep.txt"),
            to: format!("{locked}/renamed.txt"),
        },
    );
    assert!(
        matches!(
            outcome.first_failure(),
            Some(BrowseFailure::PermissionDenied { .. })
        ),
        "a rename without write permission is refused with the reason: {outcome}"
    );
    assert!(
        server_has(&server, &format!("{locked}/keep.txt"))
            && !server_has(&server, &format!("{locked}/renamed.txt")),
        "the refused rename left the entry as it was"
    );

    server
        .as_user(&format!("chmod 700 '{locked}'"))
        .expect("the fixture is made writable again");

    // The terminal was never involved, and the connection still serves both.
    assert_eq!(
        listed_names(&mut link, 7, &home),
        server_names(&server, &home),
        "the list still matches the server"
    );
    shell_echo(&mut session, "shellune-after-delete");
    session.close();
}

#[test]
fn a_change_to_a_path_that_does_not_exist_is_reported_as_missing() {
    if skip("changing a missing path") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let home = server.home();
    let missing = format!("{home}/no-such-entry");
    let mut session = live(server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");

    let outcome = change(
        &mut link,
        BrowseRequest::Delete {
            request: 1,
            path: missing.clone(),
            kind: FileKind::File,
        },
    );
    assert!(!outcome.succeeded());
    assert!(
        matches!(
            outcome.first_failure(),
            Some(BrowseFailure::NotFound { .. })
        ),
        "a missing path is named as missing: {outcome}"
    );

    let outcome = change(
        &mut link,
        BrowseRequest::Rename {
            request: 2,
            from: missing.clone(),
            to: format!("{home}/other"),
        },
    );
    assert!(!outcome.succeeded());
    assert!(matches!(
        outcome.first_failure(),
        Some(BrowseFailure::NotFound { .. })
    ));
    session.close();
}

#[test]
fn an_empty_directory_is_empty_and_the_shell_stays_authenticated() {
    if !Harness::is_available() {
        eprintln!(
            "SKIPPED listing an empty directory: this host cannot start a disposable OpenSSH \
             server, so nothing is verified here and nothing is claimed."
        );
        return;
    }
    // The accepted-auth lines live in this sshd's own log. The server named "sftp" is
    // shared by the other cases in this process, so their logins land in that count
    // whenever the tests run together. This case needs a log nobody else writes.
    let server = own_server("sftp-empty");
    let server = &server;
    let home = server.home();
    let empty = format!("{home}/empty-shellune");
    server
        .as_user(&format!("rm -rf '{empty}' && mkdir -p '{empty}'"))
        .expect("the empty directory exists");
    let before = accepted_authentications(server);
    let mut session = live(server);
    let mut link = session
        .take_file_link()
        .expect("a live session is browsable");
    let entries = list(&mut link, 1, &empty).expect("the empty directory is listed");
    assert!(
        entries.is_empty(),
        "an empty directory has no entries: {:?}",
        named(&entries)
    );
    assert_eq!(
        accepted_authentications(server),
        before + 1,
        "one authentication for the terminal, none for the empty directory"
    );
    shell_echo(&mut session, "shellune-empty-dir");
    session.close();
}
