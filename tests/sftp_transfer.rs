//! Uploading and downloading over the shell's own connection (R7, R10, R17).
//!
//! Every case here talks to the disposable OpenSSH server from
//! scripts/ssh-test-server.sh through a real connection: the terminal logs in with a
//! password and the transfer tasks run on that same connection. Nothing in this file
//! stands in for the transport, and no result is compared with itself:
//!
//! * what was uploaded is read back through a second transfer and compared byte for
//!   byte, and the server's own tools report the name and the size it really has;
//! * a name with Chinese characters and spaces is used on both sides;
//! * a target that already exists is left byte-for-byte untouched until the answer says
//!   to replace it, and the directory is listed afterwards to look for a temporary file;
//! * a cancellation, a refused directory and a connection that ends mid-transfer are
//!   observed as the failures they are, and never as a completed transfer;
//! * the shell is used again while a transfer runs, so "the terminal stays usable" is an
//!   observation rather than a claim.
//!
//! Each case works in a directory of its own, because the tests run in parallel on one
//! server and a leftover from one of them must not be read as evidence about another.
//!
//! Anywhere this host cannot start such a server the test prints why and returns: a run
//! without a controllable server never claims transfers were verified.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use shellune_core::{
    AuthMethod, OverwriteChoice, TransferDirection, TransferEvent, TransferFailure, TransferId,
    TransferUpdate,
};
use shellune_ssh::testserver::Harness;
use shellune_ssh::{
    ConnectRequest, KnownHostKey, PtySize, RemoteEvent, RemoteSession, TransferCommand,
    TransferLink, TransferRequest,
};

/// How long any single wait may take.
const PATIENCE: Duration = Duration::from_secs(90);

/// A file name with both characters that used to break paths.
const CHINESE_FILE: &str = "报告 终稿.txt";

/// The server the ordinary cases share, started once.
fn shared() -> &'static Option<Harness> {
    static SERVER: std::sync::OnceLock<Option<Harness>> = std::sync::OnceLock::new();
    SERVER.get_or_init(|| {
        if !Harness::is_available() {
            return None;
        }
        Harness::start("transfer", Harness::free_port()).ok()
    })
}

/// What to do when this host cannot run the server.
fn skip(what: &str) -> bool {
    if shared().is_some() {
        return false;
    }
    eprintln!(
        "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so no \
         transfer behaviour is verified here and none is claimed."
    );
    true
}

/// Runs one script as the server's account, one at a time.
///
/// The harness writes its script to a single file per server, so two calls at once would
/// run each other's commands. The cases run in parallel, so every call is serialized
/// here: a test's own fixtures and listings are then really its own.
fn shell(server: &Harness, script: &str) -> Result<String, String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let answer = server.as_user(script);
    drop(guard);
    answer
}

/// A directory of this case's own, created as the account the tests log in as.
fn own_directory(server: &Harness, name: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros())
        .unwrap_or(0);
    let directory = format!("{}/files-{name}-{stamp}", server.home());
    shell(server, &format!("mkdir -p '{directory}'"))
        .unwrap_or_else(|reason| panic!("the directory {directory} could not be made: {reason}"));
    directory
}

/// A server of this test's own, so nothing else writes into its places.
fn own_server(name: &str) -> Harness {
    Harness::start(name, Harness::free_port())
        .unwrap_or_else(|reason| panic!("the server {name} could not be started: {reason}"))
}

/// Connects with the identity the server's own key really has.
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

/// A live session and its transfer link.
fn live_with_link(server: &Harness) -> (RemoteSession, TransferLink) {
    let mut session = live(server);
    let link = session
        .take_transfer_link()
        .expect("a live session can move files");
    (session, link)
}

/// Every update the connection reported, kept so several tasks can be followed at once.
#[derive(Default)]
struct Recorder {
    events: Vec<TransferEvent>,
}

impl Recorder {
    /// Takes everything the connection reported since the last call.
    fn collect(&mut self, link: &mut TransferLink) -> usize {
        let events = link.drain();
        let count = events.len();
        self.events.extend(events);
        count
    }

    /// Every update of one task, in order.
    fn updates(&self, id: TransferId) -> Vec<&TransferUpdate> {
        self.events
            .iter()
            .filter(|event| event.transfer == id)
            .map(|event| &event.update)
            .collect()
    }

    /// Whether one task reported a finished outcome.
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

    /// Whether one task was asked about an existing target.
    fn asked(&self, id: TransferId) -> bool {
        self.updates(id)
            .iter()
            .any(|update| matches!(update, TransferUpdate::TargetExists { .. }))
    }

    /// Waits until one task finished, and returns the update it finished with.
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
                "the transfer neither finished nor failed in time: {:?}",
                self.updates(id)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until one task was asked about an existing target.
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

    /// Waits until one task reported progress.
    fn until_progress(&mut self, link: &mut TransferLink, id: TransferId) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.collect(link);
            if self
                .updates(id)
                .iter()
                .any(|update| matches!(update, TransferUpdate::Progress { .. }))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the transfer never reported progress: {:?}",
                self.updates(id)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Starts one transfer on the connection and returns its identifier.
fn start(
    link: &mut TransferLink,
    direction: TransferDirection,
    source: &str,
    destination: &str,
) -> TransferId {
    let id = TransferId::new();
    assert!(
        link.send(TransferCommand::Start(TransferRequest {
            transfer: id,
            direction,
            source: source.to_owned(),
            destination: destination.to_owned(),
        })),
        "the transfer reaches the connection"
    );
    id
}

/// Asks for a decision about an existing target.
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

/// Bytes a test sends: every byte value, so a text-mode accident would show up.
fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

/// A directory under this platform's temporary directory that the test removes itself.
fn scratch_directory(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let directory = std::env::temp_dir().join(format!(
        "shellune-transfer-{name}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// Writes a local file and returns its path.
fn local_file(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, bytes).expect("the local file can be written");
    path
}

/// The names the server really has in one directory.
fn server_listing(server: &Harness, directory: &str) -> Vec<String> {
    let printed = shell(server, &format!("ls -A '{directory}' || true"))
        .expect("the server lists the directory");
    printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether the server still has a directory at one path.
fn server_is_directory(server: &Harness, path: &str) -> bool {
    let printed = shell(server, &format!("test -d '{path}' && echo yes || echo no"))
        .expect("the server answers a directory test");
    printed.trim() == "yes"
}

/// The size the server itself reports for one path.
fn server_size(server: &Harness, path: &str) -> Option<u64> {
    let printed = shell(server, &format!("stat -c %s '{path}' 2>/dev/null || true"))
        .expect("the server answers stat");
    printed.trim().parse().ok()
}

/// Uses the shell again and returns everything it printed.
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
            "the terminal did not answer while a transfer ran; it printed {printed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a listing holds a temporary file of the transfer worker.
fn has_partial(listing: &[String]) -> bool {
    listing.iter().any(|name| name.contains(".shellune-part-"))
}

#[test]
fn an_upload_lands_with_exactly_the_same_bytes_under_a_chinese_name_with_spaces() {
    if skip("uploading one file") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "upload");
    let remote = format!("{directory}/{CHINESE_FILE}");
    let bytes = sample(300 * 1024);
    let scratch = scratch_directory("upload");
    let local = local_file(&scratch, CHINESE_FILE, &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );

    // The terminal keeps answering while the bytes move.
    shell_echo(&mut session, "shellune-during-upload");

    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Completed { bytes: moved } => assert_eq!(
            moved,
            bytes.len() as u64,
            "the report names the bytes that were sent"
        ),
        other => panic!("the upload did not complete: {other:?}"),
    }

    // Independent check 1: the server's own name for the file and its own size.
    let listing = server_listing(server, &directory);
    assert!(
        listing.contains(&CHINESE_FILE.to_owned()),
        "the server has the file under the exact name that was sent: {listing:?}"
    );
    assert!(
        !has_partial(&listing),
        "no temporary file is left behind: {listing:?}"
    );
    assert_eq!(
        server_size(server, &remote),
        Some(bytes.len() as u64),
        "the server reports the same size"
    );

    // Independent check 2: read the content back through a download and compare bytes.
    let back_directory = scratch_directory("upload-back");
    let back = back_directory.join("read-back.txt");
    let down = start(
        &mut link,
        TransferDirection::Download,
        &remote,
        back.to_string_lossy().as_ref(),
    );
    assert!(
        matches!(
            recorder.until_finished(&mut link, down),
            TransferUpdate::Completed { .. }
        ),
        "the read-back download completes"
    );
    let read_back = std::fs::read(&back).expect("the downloaded file exists");
    assert_eq!(
        read_back, bytes,
        "the content that comes back is the content that went out, byte for byte"
    );

    shell_echo(&mut session, "shellune-after-upload");
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(&back_directory);
}

#[test]
fn a_download_writes_the_servers_bytes_into_the_file_that_was_asked_for() {
    if skip("downloading one file") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "download");
    let bytes = sample(200 * 1024);
    let source = scratch_directory("download-source");
    // The file reaches the server through this product's own upload, so the bytes it
    // holds are known exactly; a shell heredoc would have to escape them.
    let local = local_file(&source, CHINESE_FILE, &bytes);
    let remote = format!("{directory}/源 文件.bin");

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let seed = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    assert!(
        matches!(
            recorder.until_finished(&mut link, seed),
            TransferUpdate::Completed { .. }
        ),
        "the fixture upload completes"
    );

    let target_directory = scratch_directory("download");
    let folder = target_directory.to_string_lossy().into_owned();
    let target = format!("{folder}/{CHINESE_FILE}");
    let id = start(&mut link, TransferDirection::Download, &remote, &target);
    shell_echo(&mut session, "shellune-during-download");
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Completed { bytes: moved } => assert_eq!(moved, bytes.len() as u64),
        other => panic!("the download did not complete: {other:?}"),
    }

    let written = std::fs::read(target_directory.join(CHINESE_FILE)).expect("the file was written");
    assert_eq!(written, bytes, "the local file holds the server's bytes");
    let left: Vec<String> = target_directory
        .read_dir()
        .expect("the folder is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !left.iter().any(|name| name.contains(".shellune-part-")),
        "no temporary file is left in the folder: {left:?}"
    );
    session.close();
    let _ = std::fs::remove_dir_all(&target_directory);
    let _ = std::fs::remove_dir_all(&source);
}

#[test]
fn an_existing_target_is_untouched_until_the_answer_says_to_replace_it() {
    if skip("the overwrite question") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "overwrite");
    let remote = format!("{directory}/existing.txt");
    let original = b"the original content that must survive".to_vec();
    let replacement = b"the content that replaces it, and is longer than the first".to_vec();
    let scratch = scratch_directory("overwrite");
    let first = local_file(&scratch, "first.txt", &original);
    let second = local_file(&scratch, "second.txt", &replacement);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let seed = start(
        &mut link,
        TransferDirection::Upload,
        first.to_string_lossy().as_ref(),
        &remote,
    );
    assert!(matches!(
        recorder.until_finished(&mut link, seed),
        TransferUpdate::Completed { .. }
    ));

    // The second upload finds the target and stops before writing anything.
    let id = start(
        &mut link,
        TransferDirection::Upload,
        second.to_string_lossy().as_ref(),
        &remote,
    );
    recorder.until_asked(&mut link, id);

    // While the question is open the target still holds exactly the original bytes.
    let unchanged = shell(server, &format!("cat '{remote}'")).expect("the server reads the file");
    assert_eq!(
        unchanged.as_bytes(),
        original.as_slice(),
        "the target is untouched while the question is open"
    );
    assert!(
        server_size(server, &remote) == Some(original.len() as u64),
        "the target still has its own size while the question is open"
    );

    // Keeping it ends the transfer without writing anything.
    decide(&mut link, id, OverwriteChoice::KeepExisting);
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Cancelled { residue } => assert_eq!(
            residue, None,
            "keeping the existing file leaves no partial file of its own"
        ),
        other => panic!("keeping the existing file must end as cancelled: {other:?}"),
    }
    let kept = shell(server, &format!("cat '{remote}'")).expect("the server reads the file");
    assert_eq!(
        kept.as_bytes(),
        original.as_slice(),
        "keeping the file leaves its content exactly as it was"
    );
    assert!(
        !has_partial(&server_listing(server, &directory)),
        "the temporary file of a kept target is gone"
    );

    // Replacing it is a new task, and this time the content really changes.
    let replace = start(
        &mut link,
        TransferDirection::Upload,
        second.to_string_lossy().as_ref(),
        &remote,
    );
    recorder.until_asked(&mut link, replace);
    decide(&mut link, replace, OverwriteChoice::Replace);
    assert!(
        matches!(
            recorder.until_finished(&mut link, replace),
            TransferUpdate::Completed { .. }
        ),
        "the confirmed replacement completes"
    );
    let replaced = shell(server, &format!("cat '{remote}'")).expect("the server reads the file");
    assert_eq!(
        replaced.as_bytes(),
        replacement.as_slice(),
        "the target now holds the new content"
    );
    assert!(
        !has_partial(&server_listing(server, &directory)),
        "no temporary file survives a completed replacement"
    );
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_cancelled_upload_leaves_no_partial_file_and_no_target() {
    if skip("cancelling a transfer") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "cancel");
    let remote = format!("{directory}/cancelled.bin");
    // Large enough that the transfer is still running when the cancel arrives.
    let bytes = sample(24 * 1024 * 1024);
    let scratch = scratch_directory("cancel");
    let local = local_file(&scratch, "big.bin", &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    recorder.until_progress(&mut link, id);
    cancel(&mut link, id);
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Cancelled { residue } => assert_eq!(
            residue, None,
            "a cancelled transfer cleans its temporary file up"
        ),
        other => panic!("a cancelled upload must end cancelled: {other:?}"),
    }
    let listing = server_listing(server, &directory);
    assert!(
        !listing.iter().any(|name| name == "cancelled.bin"),
        "the target of a cancelled transfer does not exist: {listing:?}"
    );
    assert!(
        !has_partial(&listing),
        "no partial file is left: {listing:?}"
    );
    shell_echo(&mut session, "shellune-after-cancel");
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_directory_the_account_may_not_write_to_is_reported_as_refused() {
    if skip("a refused directory") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = format!("{}/{}-readonly", server.home(), "files-refused");
    let remote = format!("{directory}/refused.txt");
    shell(
        server,
        &format!(
            "set -e
mkdir -p '{directory}'
chmod 555 '{directory}'"
        ),
    )
    .expect("a directory the account may not write to can be made");
    let bytes = sample(4096);
    let scratch = scratch_directory("refused");
    let local = local_file(&scratch, "refused.txt", &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Failed { failure, .. } => assert_eq!(
            failure,
            TransferFailure::PermissionDenied,
            "a refused directory is reported as a permission failure"
        ),
        other => panic!("a refused directory must not complete: {other:?}"),
    }
    assert!(
        server_size(server, &remote).is_none(),
        "nothing was written into the refused directory"
    );
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_target_that_cannot_be_replaced_is_reported_without_claiming_success() {
    if skip("a failed replacement") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "finalize");
    // The target of the upload is a directory, so the confirmed replacement cannot
    // remove it: the bytes arrive, the final name does not become the file, and the
    // report has to say exactly that instead of calling the transfer complete.
    let remote = format!("{directory}/occupied");
    shell(server, &format!("mkdir -p '{remote}'")).expect("a directory can be made");
    let bytes = sample(64 * 1024);
    let scratch = scratch_directory("finalize");
    let local = local_file(&scratch, "occupied.txt", &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    // The directory is an existing target, so it is asked about first.
    recorder.until_asked(&mut link, id);
    decide(&mut link, id, OverwriteChoice::Replace);
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Failed { failure, residue } => {
            assert_eq!(
                failure,
                TransferFailure::FinalizeFailed,
                "a target that cannot be replaced is a finalize failure"
            );
            if let Some(residue) = residue {
                let listing = server_listing(server, &directory);
                let name = residue.rsplit('/').next().unwrap_or_default().to_owned();
                assert!(
                    listing.contains(&name),
                    "a residue that is reported really exists: {listing:?}"
                );
            }
        }
        other => panic!("a failed replacement must not complete: {other:?}"),
    }
    // The target is still what it was: a directory, not a file with the content.
    assert!(
        server_is_directory(server, &remote),
        "the target was not replaced by the file"
    );
    assert!(
        server_listing(server, &directory).contains(&"occupied".to_owned()),
        "the directory named like the target is still in the listing"
    );
    session.close();
    let _ = shell(server, &format!("rm -rf '{directory}'"));
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_disk_that_is_really_full_is_reported_as_having_no_space() {
    if skip("a full disk") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let mount_point = format!("/mnt/shellune-full-{}", std::process::id());
    // A real, small file system: the write has to fail because the kernel says the disk
    // is full, not because the client decided to pretend it was.
    let mounted = server.as_root(&format!(
        "set -e
mkdir -p '{mount_point}'
mount -t tmpfs -o size=1M tmpfs '{mount_point}'
chown {user}: '{mount_point}'",
        user = server.user()
    ));
    let Ok(_) = mounted else {
        eprintln!(
            "SKIPPED a full disk: this host cannot mount a small file system inside the \
             server's environment, so nothing about a full disk is verified here."
        );
        return;
    };
    let remote = format!("{mount_point}/too-big.bin");
    let bytes = sample(3 * 1024 * 1024);
    let scratch = scratch_directory("full");
    let local = local_file(&scratch, "too-big.bin", &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    let finish = recorder.until_finished(&mut link, id);
    let listing = shell(server, &format!("ls -A '{mount_point}' || true")).unwrap_or_default();
    // Whatever the worker said, the target cannot be a complete copy of the file.
    assert!(
        server_size(server, &remote).is_none_or(|size| size < bytes.len() as u64),
        "a full disk cannot produce a complete target"
    );
    match finish {
        TransferUpdate::Failed { failure, residue } => {
            assert_eq!(
                failure,
                TransferFailure::NoSpace,
                "a full disk is reported as having no space"
            );
            if let Some(residue) = residue {
                assert!(
                    listing.contains(&residue.rsplit('/').next().unwrap_or("").to_owned()),
                    "a residue that is reported is really there: {listing:?}"
                );
            }
        }
        other => panic!("a full disk must not complete a transfer: {other:?}"),
    }
    let _ = shell(server, &format!("umount '{mount_point}'"));
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_connection_that_ends_mid_transfer_never_reports_completion() {
    if skip("a connection that ends") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "interrupted");
    let remote = format!("{directory}/interrupted.bin");
    let bytes = sample(32 * 1024 * 1024);
    let scratch = scratch_directory("interrupted");
    let local = local_file(&scratch, "interrupted.bin", &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    recorder.until_progress(&mut link, id);

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
        "a connection that ended must never produce a completed transfer: {:?}",
        recorder.updates(id)
    );
    // The target is not presented as finished content: either it does not exist or its
    // size is not the size of the file that was being sent.
    if let Some(size) = server_size(server, &remote) {
        assert_ne!(
            size,
            bytes.len() as u64,
            "the target cannot be the whole file if the transfer was interrupted"
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn two_transfers_run_side_by_side_on_one_connection() {
    if skip("two transfers at once") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "parallel");
    let first_bytes = sample(512 * 1024);
    let second_bytes = sample(300 * 1024);
    let scratch = scratch_directory("parallel");
    let first = local_file(&scratch, "first.bin", &first_bytes);
    let second = local_file(&scratch, "second.bin", &second_bytes);
    let first_remote = format!("{directory}/parallel-one.bin");
    let second_remote = format!("{directory}/parallel-two.bin");

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let first_id = start(
        &mut link,
        TransferDirection::Upload,
        first.to_string_lossy().as_ref(),
        &first_remote,
    );
    let second_id = start(
        &mut link,
        TransferDirection::Upload,
        second.to_string_lossy().as_ref(),
        &second_remote,
    );
    assert_ne!(first_id, second_id);

    assert!(
        matches!(
            recorder.until_finished(&mut link, first_id),
            TransferUpdate::Completed { .. }
        ),
        "the first transfer completes"
    );
    assert!(
        matches!(
            recorder.until_finished(&mut link, second_id),
            TransferUpdate::Completed { .. }
        ),
        "the second transfer completes, even while the first was running"
    );
    assert_eq!(
        server_size(server, &first_remote),
        Some(first_bytes.len() as u64)
    );
    assert_eq!(
        server_size(server, &second_remote),
        Some(second_bytes.len() as u64)
    );
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn every_event_names_the_task_it_belongs_to() {
    if skip("task isolation") {
        return;
    }
    // Which task an event may change is decided in shellune-core and tested there; what
    // this case adds is that the worker really tags each of its reports with the
    // identifier of the task that produced it, on a live connection.
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "isolation");
    let bytes = sample(64 * 1024);
    let scratch = scratch_directory("isolation");
    let local = local_file(&scratch, "tagged.bin", &bytes);

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let id = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &format!("{directory}/tagged.bin"),
    );
    assert!(matches!(
        recorder.until_finished(&mut link, id),
        TransferUpdate::Completed { .. }
    ));
    assert!(
        recorder.events.iter().all(|event| event.transfer == id),
        "every event names the task it belongs to, and only that task"
    );
    session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_transfer_of_one_connection_is_never_reported_on_another() {
    if skip("session identity") {
        return;
    }
    let first_server = own_server("transfer-first");
    let second_server = own_server("transfer-second");
    let bytes = sample(32 * 1024);
    let scratch = scratch_directory("two-sessions");
    let local = local_file(&scratch, "session.bin", &bytes);

    let (mut first_session, mut first_link) = live_with_link(&first_server);
    let (_second_session, mut second_link) = live_with_link(&second_server);
    let first_directory = own_directory(&first_server, "one");
    let mut recorder = Recorder::default();
    let first_id = start(
        &mut first_link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &format!("{first_directory}/session.bin"),
    );
    assert!(matches!(
        recorder.until_finished(&mut first_link, first_id),
        TransferUpdate::Completed { .. }
    ));
    assert!(
        second_link.drain().is_empty(),
        "a transfer of one connection is never reported on another"
    );
    first_session.close();
    let _ = std::fs::remove_dir_all(&scratch);
}
