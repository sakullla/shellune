//! Copying files with the traditional SCP protocol, against a real OpenSSH server (R10).
//!
//! The server these cases talk to is the disposable one from
//! `scripts/ssh-test-server.sh`, and one of them is started through
//! [Harness::start_without_sftp]: a real sshd that really refuses the SFTP subsystem, so
//! "SCP works where SFTP does not" is an observation instead of a claim. Every result is
//! compared with something the server printed itself:
//!
//! * an uploaded file is read back through the server's own `sha256sum`, and a download
//!   is compared byte for byte with what was uploaded;
//! * a directory copied with recursion is compared with `find` on the server, and a
//!   refused non-recursive copy is checked to have created nothing;
//! * names with spaces, Chinese characters, quotes and semicolons are used on both
//!   sides, and the server's listing is compared with the file that was asked for - a
//!   name that had been split or executed would leave something else behind;
//! * an existing target stays byte-for-byte untouched until the answer says to replace
//!   it; a cancellation, a refused write and a lost connection are observed as the
//!   failures they are and never as a completed copy;
//! * the SFTP path still works on the ordinary server, so the two protocols are shown to
//!   be two choices over one connection rather than a replacement.
//!
//! Anywhere this host cannot start such a server the case prints why and returns: a run
//! without a controllable server never claims SCP was verified.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use shellune_core::{
    AuthMethod, OverwriteChoice, SessionId, Transfer, TransferDirection, TransferEvent,
    TransferFailure, TransferId, TransferProtocol, TransferUpdate,
};
use shellune_ssh::testserver::Harness;
use shellune_ssh::{
    ConnectRequest, KnownHostKey, PtySize, RemoteEvent, RemoteSession, TransferCommand,
    TransferLink, TransferRequest,
};

/// How long any single wait may take.
const PATIENCE: Duration = Duration::from_secs(90);

/// A file name with the characters that used to break a copied path.
const AWKWARD: &str = "报告 '终稿' 与;空格.txt";

/// The ordinary server, started once.
fn shared() -> &'static Option<Harness> {
    static SERVER: std::sync::OnceLock<Option<Harness>> = std::sync::OnceLock::new();
    SERVER.get_or_init(|| {
        if !Harness::is_available() {
            return None;
        }
        Harness::start("scp", Harness::free_port()).ok()
    })
}

/// A server without an SFTP subsystem, started once.
///
/// It is the case this whole module exists for: SCP has to work where the file browser
/// cannot.
fn without_sftp() -> &'static Option<Harness> {
    static SERVER: std::sync::OnceLock<Option<Harness>> = std::sync::OnceLock::new();
    SERVER.get_or_init(|| {
        if !Harness::is_available() {
            return None;
        }
        Harness::start_without_sftp("scp-no-sftp", Harness::free_port()).ok()
    })
}

/// What to do when this host cannot run the server.
fn skip(what: &str, server: &Option<Harness>) -> bool {
    if server.is_some() {
        return false;
    }
    eprintln!(
        "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so no SCP \
         behaviour is verified here and none is claimed."
    );
    true
}

/// Runs one script as the server's account, one at a time.
fn shell(server: &Harness, script: &str) -> Result<String, String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let answer = server.as_user(script);
    drop(guard);
    answer
}

/// A directory of this case's own on the server.
fn own_server_directory(server: &Harness, name: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros())
        .unwrap_or(0);
    let directory = format!("{}/scp-{name}-{stamp}", server.home());
    shell(server, &format!("mkdir -p {}", quoted(&directory)))
        .unwrap_or_else(|reason| panic!("the directory {directory} could not be made: {reason}"));
    directory
}

/// A directory under this machine's temporary directory that the case removes itself.
fn scratch_directory(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let directory = std::env::temp_dir().join(format!(
        "shellune-scp-{name}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// Connects with the identity the server's own key really has.
fn live(server: &Harness) -> RemoteSession {
    let mut session = RemoteSession::connect(ConnectRequest::new(
        "127.0.0.1",
        server.port(),
        server.user(),
        AuthMethod::password(server.password()),
        PtySize::new(100, 30),
        KnownHostKey::Trusted {
            fingerprint: server.host_key_fingerprint(),
        },
    ));
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

/// A live session and its transfer link.
fn live_with_link(server: &Harness) -> (RemoteSession, TransferLink) {
    let mut session = live(server);
    let link = session
        .take_transfer_link()
        .expect("a live session can copy files");
    (session, link)
}

/// Every update the connection reported, kept so several tasks can be followed.
#[derive(Default)]
struct Recorder {
    events: Vec<TransferEvent>,
}

impl Recorder {
    fn collect(&mut self, link: &mut TransferLink) -> usize {
        let events = link.drain();
        let count = events.len();
        self.events.extend(events);
        count
    }

    fn updates(&self, id: TransferId) -> Vec<&TransferUpdate> {
        self.events
            .iter()
            .filter(|event| event.transfer == id)
            .map(|event| &event.update)
            .collect()
    }

    fn finished(&self, id: TransferId) -> bool {
        self.updates(id).iter().any(|update| {
            matches!(
                update,
                TransferUpdate::Completed { .. }
                    | TransferUpdate::Failed { .. }
                    | TransferUpdate::Cancelled { .. }
            )
        })
    }

    fn asked(&self, id: TransferId) -> bool {
        self.updates(id)
            .iter()
            .any(|update| matches!(update, TransferUpdate::TargetExists { .. }))
    }

    /// Waits until one task reported an outcome, and returns it.
    fn until_finished(&mut self, link: &mut TransferLink, id: TransferId) -> TransferUpdate {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.collect(link);
            if self.finished(id) {
                return self
                    .updates(id)
                    .into_iter()
                    .next_back()
                    .cloned()
                    .expect("the task reported an update");
            }
            assert!(
                Instant::now() < deadline,
                "the copy neither finished nor failed in time: {:?}",
                self.updates(id)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn until_asked(&mut self, link: &mut TransferLink, id: TransferId) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.collect(link);
            if self.asked(id) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the existing target was never reported: {:?}",
                self.updates(id)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until one task reported progress of at least `bytes` bytes.
    fn until_progress(&mut self, link: &mut TransferLink, id: TransferId, bytes: u64) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.collect(link);
            let reached = self.updates(id).iter().any(|update| {
                matches!(
                    update,
                    TransferUpdate::Progress { transferred, .. } if *transferred >= bytes
                )
            });
            if reached {
                return;
            }
            assert!(
                !self.finished(id),
                "the copy finished before it reported that much progress: {:?}",
                self.updates(id)
            );
            assert!(
                Instant::now() < deadline,
                "no progress of {bytes} bytes arrived: {:?}",
                self.updates(id)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// One task specification, so the helpers stay readable.
struct Ask<'a> {
    direction: TransferDirection,
    protocol: TransferProtocol,
    source: &'a str,
    destination: &'a str,
    recursive: bool,
}

impl<'a> Ask<'a> {
    fn upload(source: &'a str, destination: &'a str) -> Self {
        Self {
            direction: TransferDirection::Upload,
            protocol: TransferProtocol::Scp,
            source,
            destination,
            recursive: false,
        }
    }

    fn download(source: &'a str, destination: &'a str) -> Self {
        Self {
            direction: TransferDirection::Download,
            protocol: TransferProtocol::Scp,
            source,
            destination,
            recursive: false,
        }
    }

    fn recursive(mut self) -> Self {
        self.recursive = true;
        self
    }

    fn over_sftp(mut self) -> Self {
        self.protocol = TransferProtocol::Sftp;
        self
    }
}

/// Starts one copy on the connection and returns its identifier.
fn start(link: &mut TransferLink, ask: Ask<'_>) -> TransferId {
    let task = Transfer::queued_over(
        SessionId::new(),
        ask.protocol,
        ask.direction,
        ask.source,
        ask.destination,
    )
    .with_recursive(ask.recursive);
    let id = task.id();
    assert!(
        link.send(TransferCommand::Start(TransferRequest::for_transfer(&task))),
        "the copy reaches the connection"
    );
    id
}

/// Answers the question about an existing target.
fn decide(link: &mut TransferLink, id: TransferId, choice: OverwriteChoice) {
    assert!(
        link.send(TransferCommand::Decide {
            transfer: id,
            choice
        }),
        "the decision reaches the connection"
    );
}

/// Asks one task to stop.
fn cancel(link: &mut TransferLink, id: TransferId) {
    assert!(
        link.send(TransferCommand::Cancel { transfer: id }),
        "the cancellation reaches the connection"
    );
}

/// Bytes a test uploads: every byte value, so a text-mode accident would show up.
fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

/// The SHA-256 the server itself prints for one path.
fn server_sha256(server: &Harness, path: &str) -> Option<String> {
    let printed = shell(server, &format!("sha256sum {} 2>/dev/null", quoted(path)))
        .expect("the server answers sha256sum");
    printed.split_whitespace().next().map(str::to_owned)
}

/// The size the server itself reports for one path.
fn server_size(server: &Harness, path: &str) -> Option<u64> {
    let printed = shell(
        server,
        &format!("stat -c %s {} 2>/dev/null || true", quoted(path)),
    )
    .expect("the server answers stat");
    printed.trim().parse().ok()
}

/// One path as a shell word, so a name with a quote in it stays one argument.
fn quoted(path: &str) -> String {
    format!("'{}'", path.replace('\'', r#"'\''"#))
}

/// The names the server really has in one directory.
fn server_listing(server: &Harness, directory: &str) -> Vec<String> {
    let printed =
        shell(server, &format!("ls -A {} || true", quoted(directory))).expect("the server lists");
    printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether the server still has anything at one path.
fn server_has(server: &Harness, path: &str) -> bool {
    let printed = shell(
        server,
        &format!("test -e {} && echo yes || echo no", quoted(path)),
    )
    .expect("the server answers an existence test");
    printed.trim() == "yes"
}

#[test]
fn a_file_goes_up_and_comes_down_over_a_server_without_sftp() {
    let Some(server) = without_sftp().as_ref() else {
        if skip("a file copy over SCP", without_sftp()) {
            return;
        }
        unreachable!()
    };
    // The server really has no SFTP: that is what makes this a test of SCP.
    assert!(
        !server_config_mentions_sftp(server),
        "the disposable server must offer no SFTP subsystem"
    );
    let upload_directory = own_server_directory(server, "roundtrip");
    let local = scratch_directory("roundtrip");
    let bytes = sample(70_000);
    let source = local.join(AWKWARD);
    std::fs::write(&source, &bytes).expect("the local file can be written");
    let target = format!("{upload_directory}/{AWKWARD}");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    let outcome = recorder.until_finished(&mut link, id);
    match &outcome {
        TransferUpdate::Completed { bytes: written } => {
            assert_eq!(*written, bytes.len() as u64, "the whole file was copied");
        }
        other => panic!("the upload did not complete: {other:?}"),
    }
    assert_eq!(
        server_size(server, &target),
        Some(bytes.len() as u64),
        "the server holds a file of exactly the uploaded size"
    );
    assert_eq!(
        server_listing(server, &upload_directory),
        vec![AWKWARD.to_owned()],
        "the awkward name arrived as one name and nothing else was created"
    );

    // And back down again, through a second task on the same connection.
    let downloaded = local.join("downloaded.txt");
    let id = start(
        &mut link,
        Ask::download(&target, downloaded.to_str().expect("a utf-8 path")),
    );
    let outcome = recorder.until_finished(&mut link, id);
    assert!(
        matches!(outcome, TransferUpdate::Completed { .. }),
        "the download did not complete: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(&downloaded).expect("the downloaded file exists"),
        bytes,
        "what came down is byte for byte what went up"
    );
    let leftovers: Vec<PathBuf> = std::fs::read_dir(&local)
        .expect("the scratch directory can be read")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .map(|name| {
                    name.to_string_lossy()
                        .starts_with(shellune_core::PARTIAL_PREFIX)
                })
                .unwrap_or(false)
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "a completed download leaves no temporary file: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&local);
}

/// A name of this run's own, so two runs cannot see each other's files.
fn own_name(prefix: &str, suffix: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros())
        .unwrap_or(0);
    format!("{prefix}-{stamp}{suffix}")
}

/// Whether the disposable server's configuration offers the SFTP subsystem.
fn server_config_mentions_sftp(server: &Harness) -> bool {
    let config = std::fs::read_to_string(server.dir().join("sshd_config")).unwrap_or_default();
    config
        .lines()
        .map(str::trim)
        .any(|line| line.to_lowercase().starts_with("subsystem") && line.contains("sftp"))
}

#[test]
fn a_path_that_says_home_really_copies_into_and_out_of_the_account_home() {
    // The question a server without SFTP leaves a user with starts at `~`, because that
    // is the only remote directory the window can know without a listing. `~` is data
    // the remote shell expands, so the sink has to be started in the real home - not in
    // a directory literally called `~` at the root, which is what normalizing the path
    // first would have asked for.
    let Some(server) = without_sftp().as_ref() else {
        if skip("a copy into and out of the home directory", without_sftp()) {
            return;
        }
        unreachable!()
    };
    let local = scratch_directory("home");
    let top_name = own_name("shellune-home", ".txt");
    let sub_name = own_name("scp-home-sub", "");
    let top_source = local.join("report.txt");
    let top_bytes = sample(4_096);
    std::fs::write(&top_source, &top_bytes).expect("the local file can be written");
    // A directory of its own inside the home, so the `~/sub` case is observable too.
    shell(
        server,
        &format!("mkdir -p {}/{}", quoted(&server.home()), quoted(&sub_name)),
    )
    .expect("the subdirectory can be made in the home");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();

    // The destination is exactly what the upload question is prefilled with: `~/name`.
    let home_target = format!("~/{top_name}");
    let id = start(
        &mut link,
        Ask::upload(top_source.to_str().expect("a utf-8 path"), &home_target),
    );
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Completed { bytes } => {
            assert_eq!(bytes, top_bytes.len() as u64, "the whole file went up")
        }
        other => panic!("the copy into the home directory did not complete: {other:?}"),
    }
    let real_target = format!("{}/{}", server.home(), top_name);
    assert_eq!(
        server_size(server, &real_target),
        Some(top_bytes.len() as u64),
        "the file is in the account's home, under the name that was asked for"
    );
    assert!(
        !server_has(server, &format!("/~/{top_name}")),
        "nothing was written into a directory called ~ at the root"
    );

    // A path inside the home, typed the same way, lands in that subdirectory.
    let sub_target = format!("~/{sub_name}/report.txt");
    let id = start(
        &mut link,
        Ask::upload(top_source.to_str().expect("a utf-8 path"), &sub_target),
    );
    assert!(
        matches!(
            recorder.until_finished(&mut link, id),
            TransferUpdate::Completed { .. }
        ),
        "the copy into ~/{sub_name} did not complete: {:?}",
        recorder.updates(id)
    );
    assert_eq!(
        server_listing(server, &format!("{}/{sub_name}", server.home())),
        vec!["report.txt".to_owned()],
        "the file arrived inside the subdirectory of the home"
    );

    // And it comes back down from the same `~/` path.
    let downloaded = local.join("downloaded.txt");
    let id = start(
        &mut link,
        Ask::download(&home_target, downloaded.to_str().expect("a utf-8 path")),
    );
    assert!(
        matches!(
            recorder.until_finished(&mut link, id),
            TransferUpdate::Completed { .. }
        ),
        "the download from ~/{top_name} did not complete: {:?}",
        recorder.updates(id)
    );
    assert_eq!(
        std::fs::read(&downloaded).expect("the downloaded file exists"),
        top_bytes,
        "what came out of the home is what went into it"
    );

    let _ = shell(
        server,
        &format!(
            "rm -f {} && rm -rf {}",
            quoted(&real_target),
            quoted(&format!("{}/{sub_name}", server.home()))
        ),
    );
    let _ = std::fs::remove_dir_all(&local);
}

#[test]
fn a_directory_is_copied_only_when_recursion_was_chosen() {
    // The same server the round trip uses: a real sshd that refuses the SFTP subsystem,
    // so a recursive copy that works here cannot be SFTP doing the work.
    let Some(server) = without_sftp().as_ref() else {
        if skip("a recursive directory copy", without_sftp()) {
            return;
        }
        unreachable!()
    };
    let directory = own_server_directory(server, "tree");
    let local = scratch_directory("tree");
    let source = local.join("project");
    std::fs::create_dir_all(source.join("nested").join("deeper"))
        .expect("the local tree can be created");
    std::fs::write(source.join("readme.txt"), b"top").expect("the file can be written");
    std::fs::write(source.join("nested").join("note.txt"), b"nested").expect("the file");
    std::fs::write(
        source.join("nested").join("deeper").join("deep.txt"),
        b"deep",
    )
    .expect("the file");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();

    // Without the choice, a directory source is refused and nothing is created.
    let target = format!("{directory}/project");
    let refused = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    let outcome = recorder.until_finished(&mut link, refused);
    match &outcome {
        TransferUpdate::Failed { failure, .. } => assert!(
            matches!(failure, TransferFailure::Refused { .. }),
            "a directory without recursion is refused: {failure:?}"
        ),
        other => panic!("an unasked directory copy must not start: {other:?}"),
    }
    assert!(
        !server_has(server, &target),
        "nothing was created for the refused copy"
    );

    let copied = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target).recursive(),
    );
    let outcome = recorder.until_finished(&mut link, copied);
    assert!(
        matches!(outcome, TransferUpdate::Completed { .. }),
        "the recursive copy did not complete: {outcome:?}"
    );
    assert_eq!(
        server_listing(server, &target),
        vec!["nested".to_owned(), "readme.txt".to_owned()],
        "the top of the tree arrived"
    );
    assert_eq!(
        shell(
            server,
            &format!(
                "cat {}",
                quoted(&format!("{target}/nested/deeper/deep.txt"))
            ),
        )
        .expect("the server reads the deepest file")
        .trim(),
        "deep",
        "the whole structure arrived, not just the first level"
    );

    // And the same tree comes back down, with its structure.
    let restored = local.join("restored");
    let id = start(
        &mut link,
        Ask::download(&target, restored.to_str().expect("a utf-8 path")).recursive(),
    );
    let outcome = recorder.until_finished(&mut link, id);
    assert!(
        matches!(outcome, TransferUpdate::Completed { .. }),
        "the recursive download did not complete: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(restored.join("nested").join("deeper").join("deep.txt"))
            .expect("the deepest file came down"),
        b"deep"
    );
    assert_eq!(
        std::fs::read(restored.join("readme.txt")).expect("the top file came down"),
        b"top"
    );
    // A tree's whole size is not known while it arrives, so its progress is bytes and
    // never a percentage: a total that changed per file would be smaller than what was
    // already written, and 100% of nothing is not a progress report.
    let reports: Vec<&TransferUpdate> = recorder
        .updates(id)
        .into_iter()
        .filter(|update| matches!(update, TransferUpdate::Progress { .. }))
        .collect();
    assert!(
        !reports.is_empty(),
        "the recursive download reported how far it had come: {:?}",
        recorder.updates(id)
    );
    let mut highest = 0u64;
    for update in reports.iter().copied() {
        let TransferUpdate::Progress { transferred, total } = update else {
            unreachable!("only progress updates are collected")
        };
        assert!(
            total.is_none(),
            "a tree download claims no total it cannot know: {update:?}"
        );
        assert!(
            *transferred >= highest,
            "the bytes written never go backwards: {update:?} after {highest}"
        );
        highest = *transferred;
    }
    assert_eq!(
        highest,
        b"top".len() as u64 + b"nested".len() as u64 + b"deep".len() as u64,
        "the last report is the whole tree"
    );
    assert!(
        recorder
            .updates(id)
            .into_iter()
            .next_back()
            .is_some_and(|update| matches!(update, TransferUpdate::Completed { .. })),
        "the tree is complete only after its last file: {:?}",
        recorder.updates(id)
    );
    let _ = std::fs::remove_dir_all(&local);
}

#[test]
fn an_existing_target_is_untouched_until_the_answer_replaces_it() {
    let Some(server) = shared().as_ref() else {
        if skip("an existing target", shared()) {
            return;
        }
        unreachable!()
    };
    let directory = own_server_directory(server, "existing");
    let local = scratch_directory("existing");
    let source = local.join("report.txt");
    std::fs::write(&source, b"the new content").expect("the local file can be written");
    let target = format!("{directory}/report.txt");
    shell(
        server,
        &format!("printf 'the old content' > {}", quoted(&target)),
    )
    .expect("the server writes the existing file");
    let before = server_sha256(server, &target).expect("the server hashes the target");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    recorder.until_asked(&mut link, id);
    assert_eq!(
        server_sha256(server, &target),
        Some(before.clone()),
        "nothing was written while the question was open"
    );

    decide(&mut link, id, OverwriteChoice::KeepExisting);
    let outcome = recorder.until_finished(&mut link, id);
    assert!(
        !matches!(outcome, TransferUpdate::Completed { .. }),
        "keeping the target is not a completed copy: {outcome:?}"
    );
    assert_eq!(
        server_sha256(server, &target),
        Some(before),
        "the existing file is exactly as it was"
    );

    // The same task asked again, answered with a replacement this time.
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    recorder.until_asked(&mut link, id);
    decide(&mut link, id, OverwriteChoice::Replace);
    let outcome = recorder.until_finished(&mut link, id);
    assert!(
        matches!(outcome, TransferUpdate::Completed { .. }),
        "the confirmed replacement did not complete: {outcome:?}"
    );
    assert_eq!(
        shell(server, &format!("cat {}", quoted(&target))).expect("the server reads the target"),
        "the new content",
        "the target holds the content that was copied"
    );
    let _ = std::fs::remove_dir_all(&local);
}

#[test]
fn a_write_the_server_refuses_is_reported_as_that_refusal() {
    let Some(server) = shared().as_ref() else {
        if skip("a refused write", shared()) {
            return;
        }
        unreachable!()
    };
    let read_only = own_server_directory(server, "readonly");
    shell(server, &format!("chmod 555 {}", quoted(&read_only)))
        .expect("the directory can be read-only");
    let local = scratch_directory("readonly");
    let source = local.join("report.txt");
    std::fs::write(&source, b"content").expect("the local file can be written");
    let target = format!("{read_only}/report.txt");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    let outcome = recorder.until_finished(&mut link, id);
    match &outcome {
        TransferUpdate::Failed { failure, .. } => {
            assert!(
                !failure.is_connection_lost(),
                "the server refused the write; the connection did not end: {failure:?}"
            );
        }
        other => panic!("a refused write must not look finished: {other:?}"),
    }
    assert!(
        !server_has(server, &target),
        "the refused write created nothing"
    );
    shell(server, &format!("chmod 755 {}", quoted(&read_only))).ok();
    let _ = std::fs::remove_dir_all(&local);
}

#[test]
fn a_missing_source_and_a_missing_target_directory_are_different_reasons() {
    let Some(server) = shared().as_ref() else {
        if skip("a missing path", shared()) {
            return;
        }
        unreachable!()
    };
    let directory = own_server_directory(server, "missing");
    let local = scratch_directory("missing");
    let source = local.join("report.txt");
    std::fs::write(&source, b"content").expect("the local file can be written");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();

    // A download of a path that does not exist.
    let id = start(
        &mut link,
        Ask::download(
            &format!("{directory}/absent.txt"),
            local.join("absent.txt").to_str().expect("a utf-8 path"),
        ),
    );
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Failed { failure, .. } => assert_eq!(
            failure,
            TransferFailure::NotFound,
            "a source that does not exist is reported as such"
        ),
        other => panic!("a missing source must fail: {other:?}"),
    }

    // An upload into a directory that does not exist.
    let id = start(
        &mut link,
        Ask::upload(
            source.to_str().expect("a utf-8 path"),
            &format!("{directory}/absent/report.txt"),
        ),
    );
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Failed { failure, .. } => assert!(
            matches!(
                failure,
                TransferFailure::NotFound | TransferFailure::Refused { .. }
            ),
            "a target directory that does not exist is named, not invented: {failure:?}"
        ),
        other => panic!("a missing target directory must fail: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&local);
}

#[test]
fn a_cancelled_copy_is_never_a_completed_one() {
    let Some(server) = shared().as_ref() else {
        if skip("a cancelled copy", shared()) {
            return;
        }
        unreachable!()
    };
    let directory = own_server_directory(server, "cancel");
    let local = scratch_directory("cancel");
    let source = local.join("big.bin");
    // Big enough that a cancel really arrives while bytes are moving.
    let file = std::fs::File::create(&source).expect("the local file can be made");
    file.set_len(128 * 1024 * 1024)
        .expect("the local file can be given a size");
    drop(file);
    let target = format!("{directory}/big.bin");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    // Wait until bytes really moved, then stop the task.
    recorder.until_progress(&mut link, id, 1);
    cancel(&mut link, id);
    let outcome = recorder.until_finished(&mut link, id);
    match &outcome {
        TransferUpdate::Cancelled { .. } => {}
        other => panic!("a cancelled copy must be reported as cancelled: {other:?}"),
    }
    assert!(
        !matches!(outcome, TransferUpdate::Completed { .. }),
        "a cancelled copy never claims completion"
    );
    let _ = std::fs::remove_dir_all(&local);
}

#[test]
fn a_connection_that_ends_mid_copy_never_reports_completion() {
    let Some(server) = shared().as_ref() else {
        if skip("a connection that ends", shared()) {
            return;
        }
        unreachable!()
    };
    let directory = own_server_directory(server, "interrupted");
    let local = scratch_directory("interrupted");
    let source = local.join("interrupted.bin");
    let file = std::fs::File::create(&source).expect("the local file can be made");
    // Large enough that the bytes are still moving when the session closes.
    file.set_len(96 * 1024 * 1024)
        .expect("the local file can be given a size");
    drop(file);
    let target = format!("{directory}/interrupted.bin");
    let expected = std::fs::metadata(&source).expect("the file exists").len();

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        Ask::upload(source.to_string_lossy().as_ref(), &target),
    );
    recorder.until_progress(&mut link, id, 1);

    // The user closes the session while the bytes are moving.
    session.close();
    let wait_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < wait_until {
        recorder.collect(&mut link);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !recorder
            .updates(id)
            .iter()
            .any(|update| matches!(update, TransferUpdate::Completed { .. })),
        "a connection that ended must never produce a completed copy: {:?}",
        recorder.updates(id)
    );
    // SCP writes into the target's own name, so a stopped copy leaves part of the
    // content there - and that is exactly what the report has to name.
    if let Some(size) = server_size(server, &target) {
        assert_ne!(
            size, expected,
            "the target cannot be the whole file if the copy was interrupted"
        );
    }
    let residue = cx_free_residue(&recorder, id);
    if let Some(residue) = residue {
        assert_eq!(
            residue, target,
            "the file that holds part of the content is named as it really is"
        );
    }
    let _ = shell(server, &format!("rm -rf {}", quoted(&directory)));
    let _ = std::fs::remove_dir_all(&local);
}

/// The partial file a task named, when its report carried one.
fn cx_free_residue(recorder: &Recorder, id: TransferId) -> Option<String> {
    recorder
        .updates(id)
        .into_iter()
        .rev()
        .find_map(|update| match update {
            TransferUpdate::Failed { residue, .. } | TransferUpdate::Cancelled { residue, .. } => {
                residue.clone()
            }
            _ => None,
        })
}

#[test]
fn sftp_still_copies_on_the_ordinary_server_and_scp_does_not_replace_it() {
    let Some(server) = shared().as_ref() else {
        if skip("both protocols", shared()) {
            return;
        }
        unreachable!()
    };
    let directory = own_server_directory(server, "both");
    let local = scratch_directory("both");
    let source = local.join("report.txt");
    std::fs::write(&source, b"content over either protocol")
        .expect("the local file can be written");

    let (_session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();

    // SFTP, chosen explicitly.
    let target = format!("{directory}/over-sftp.txt");
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target).over_sftp(),
    );
    let outcome = recorder.until_finished(&mut link, id);
    assert!(
        matches!(outcome, TransferUpdate::Completed { .. }),
        "the SFTP copy still works: {outcome:?}"
    );

    // And SCP beside it, on the same connection.
    let target = format!("{directory}/over-scp.txt");
    let id = start(
        &mut link,
        Ask::upload(source.to_str().expect("a utf-8 path"), &target),
    );
    let outcome = recorder.until_finished(&mut link, id);
    assert!(
        matches!(outcome, TransferUpdate::Completed { .. }),
        "the SCP copy works beside it: {outcome:?}"
    );
    assert_eq!(
        server_listing(server, &directory),
        vec!["over-scp.txt".to_owned(), "over-sftp.txt".to_owned()]
    );

    // A resume hint never reaches SCP: the protocol has nothing to continue.
    let id = TransferId::new();
    assert!(
        link.send(TransferCommand::Start(TransferRequest {
            transfer: id,
            direction: TransferDirection::Upload,
            source: source.to_string_lossy().into_owned(),
            destination: format!("{directory}/resumed.txt"),
            resume: Some(shellune_core::ResumeHint {
                partial: format!("{directory}/{}{id}", shellune_core::PARTIAL_PREFIX),
                record: local.join("record.json").to_string_lossy().into_owned(),
                offset: 1024,
                generation: 1,
            }),
            protocol: TransferProtocol::Scp,
            recursive: false,
        })),
        "the request reaches the connection"
    );
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Failed {
            failure,
            residue,
            resume,
        } => {
            assert!(
                matches!(failure, TransferFailure::ResumeRefused { .. }),
                "SCP refuses a checkpoint instead of pretending to continue: {failure:?}"
            );
            assert!(residue.is_none(), "nothing was written");
            assert!(resume.is_none(), "SCP offers no resume");
        }
        other => panic!("a checkpoint sent to SCP must be refused: {other:?}"),
    }
    assert!(
        !server_has(server, &format!("{directory}/resumed.txt")),
        "the refused resume wrote nothing"
    );
    let _ = std::fs::remove_dir_all(&local);
}
