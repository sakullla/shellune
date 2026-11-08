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
    AuthMethod, OverwriteChoice, PARTIAL_PREFIX, ResumeHint, TransferDirection, TransferEvent,
    TransferFailure, TransferId, TransferUpdate,
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

    /// Waits until one task reported a checkpoint that is still short of `total`.
    fn until_partial_checkpoint(
        &mut self,
        link: &mut TransferLink,
        id: TransferId,
        total: u64,
    ) -> ResumeHint {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.collect(link);
            let found = self
                .updates(id)
                .into_iter()
                .rev()
                .find_map(|update| match update {
                    TransferUpdate::Checkpoint(hint) if hint.offset > 0 && hint.offset < total => {
                        Some(hint.clone())
                    }
                    _ => None,
                });
            if let Some(hint) = found {
                return hint;
            }
            assert!(
                !self.finished(id),
                "the transfer finished before a partial checkpoint: {:?}",
                self.updates(id)
            );
            assert!(
                Instant::now() < deadline,
                "no partial checkpoint arrived: {:?}",
                self.updates(id)
            );
            std::thread::sleep(Duration::from_millis(5));
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
    start_from(link, direction, source, destination, None)
}

/// Starts one transfer, optionally from a checkpoint the worker must re-check.
fn start_from(
    link: &mut TransferLink,
    direction: TransferDirection,
    source: &str,
    destination: &str,
    resume: Option<ResumeHint>,
) -> TransferId {
    let id = TransferId::new();
    assert!(
        link.send(TransferCommand::Start(TransferRequest {
            transfer: id,
            direction,
            source: source.to_owned(),
            destination: destination.to_owned(),
            resume,
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

/// An upload cancelled after a confirmed prefix, with the target still absent.
struct HeldUpload {
    directory: String,
    remote: String,
    local: PathBuf,
    scratch: PathBuf,
    bytes: Vec<u8>,
    hint: ResumeHint,
    partial: String,
}

/// Uploads until a checkpoint exists, then cancels. The target must not exist.
fn hold_upload(
    server: &Harness,
    link: &mut TransferLink,
    name: &str,
    len: usize,
) -> (Recorder, HeldUpload) {
    let directory = own_directory(server, name);
    let remote = format!("{directory}/{name}.bin");
    let bytes = sample(len);
    let scratch = scratch_directory(name);
    let local = local_file(&scratch, &format!("{name}.bin"), &bytes);
    let mut recorder = Recorder::default();
    let id = start(
        link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    recorder.until_partial_checkpoint(link, id, bytes.len() as u64);
    cancel(link, id);
    let held = match recorder.until_finished(link, id) {
        TransferUpdate::Cancelled { residue, resume } => {
            let hint = resume.expect("a confirmed prefix is kept with its checkpoint");
            let partial = residue.expect("the partial file is named");
            assert!(hint.offset > 0, "the checkpoint names real bytes");
            assert!(
                hint.offset < bytes.len() as u64,
                "the cancel landed before the whole file was confirmed"
            );
            assert!(
                server_size(server, &remote).is_none(),
                "cancelling does not create the target"
            );
            let partial_size = server_size(server, &partial);
            assert!(
                partial_size.is_some_and(|size| size >= hint.offset && size < bytes.len() as u64),
                "the partial file holds the confirmed prefix and not the finished target: {partial_size:?}"
            );
            assert!(
                !recorder
                    .updates(id)
                    .iter()
                    .any(|update| matches!(update, TransferUpdate::Completed { .. })),
                "a cancelled upload is not completed"
            );
            HeldUpload {
                directory,
                remote,
                local,
                scratch,
                bytes,
                hint,
                partial,
            }
        }
        other => panic!("a cancelled upload must end cancelled: {other:?}"),
    };
    (recorder, held)
}

/// The first progress a task reported, which is the offset a resume must start from.
fn first_progress(recorder: &Recorder, id: TransferId) -> Option<u64> {
    recorder
        .updates(id)
        .into_iter()
        .find_map(|update| match update {
            TransferUpdate::Progress { transferred, .. } => Some(*transferred),
            _ => None,
        })
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
        TransferUpdate::Cancelled { residue, .. } => assert_eq!(
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
fn a_cancelled_upload_keeps_a_confirmed_prefix_and_does_not_create_the_target() {
    if skip("cancelling a transfer") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let (mut session, mut link) = live_with_link(server);
    let (_recorder, held) = hold_upload(server, &mut link, "cancel", 16 * 1024 * 1024);
    let listing = server_listing(server, &held.directory);
    assert!(
        !listing.iter().any(|name| name == "cancel.bin"),
        "the target of a cancelled transfer does not exist: {listing:?}"
    );
    assert!(
        has_partial(&listing),
        "the confirmed prefix stays so it can be checked again: {listing:?}"
    );
    shell_echo(&mut session, "shellune-after-cancel");
    session.close();
    let _ = std::fs::remove_file(&held.hint.record);
    let _ = shell(server, &format!("rm -rf '{}'", held.directory));
    let _ = std::fs::remove_dir_all(&held.scratch);
}

#[test]
fn cancelling_the_overwrite_question_writes_nothing() {
    if skip("cancelling an overwrite question") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "cancel-ask");
    let remote = format!("{directory}/existing.txt");
    let original = b"the original content that must survive a cancelled question".to_vec();
    let replacement = b"replacement bytes that must not be written".to_vec();
    let scratch = scratch_directory("cancel-ask");
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

    let id = start(
        &mut link,
        TransferDirection::Upload,
        second.to_string_lossy().as_ref(),
        &remote,
    );
    recorder.until_asked(&mut link, id);
    cancel(&mut link, id);
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Cancelled { residue, resume } => {
            assert_eq!(residue, None);
            assert!(
                resume.is_none(),
                "a question that wrote nothing has no prefix"
            );
        }
        other => panic!("cancelling the question must end cancelled: {other:?}"),
    }
    let kept = shell(server, &format!("cat '{remote}'")).expect("the server reads the file");
    assert_eq!(kept.as_bytes(), original.as_slice());
    assert!(
        !has_partial(&server_listing(server, &directory)),
        "cancelling the question leaves no temporary file"
    );
    assert!(
        !recorder.updates(id).iter().any(|update| matches!(
            update,
            TransferUpdate::Completed { .. } | TransferUpdate::Progress { .. }
        )),
        "the cancelled question never started writing: {:?}",
        recorder.updates(id)
    );
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
        TransferUpdate::Failed {
            failure, residue, ..
        } => {
            assert_eq!(
                failure,
                TransferFailure::FinalizeFailed,
                "a target that cannot be replaced is a finalize failure"
            );
            // The bytes did arrive: the complete copy is the only one there is, so it is
            // kept and named instead of being deleted with the failure.
            let residue = residue.expect(
                "a replacement that failed after the bytes were written names the complete copy",
            );
            let listing = server_listing(server, &directory);
            let name = residue.rsplit('/').next().unwrap_or_default().to_owned();
            assert!(
                listing.contains(&name),
                "a residue that is reported really exists: {listing:?}"
            );
            assert_eq!(
                server_size(server, &residue),
                Some(bytes.len() as u64),
                "the file that is named holds the whole upload, not a fragment"
            );
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
        TransferUpdate::Failed {
            failure, residue, ..
        } => {
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

    // The worker's own report cannot arrive - its task died with the runtime - so the
    // path the window reports on a lost connection is derived from the task itself. It
    // has to be the file the server really kept, or the report would send the user to a
    // place where there is nothing.
    let reported = shellune_core::partial_path(TransferDirection::Upload, &remote, id);
    let listing = server_listing(server, &directory);
    let name = reported.rsplit('/').next().unwrap_or_default().to_owned();
    assert!(
        listing.contains(&name),
        "the partial file a lost connection is reported with is the one the server left: \
         {reported} is not in {listing:?}"
    );
    assert!(
        server_size(server, &reported).is_some_and(|size| size > 0),
        "the named file holds the bytes that were already sent, that is why it is named"
    );
    assert_eq!(
        name,
        format!("{PARTIAL_PREFIX}{id}"),
        "and it is the recognizable temporary name of this very task"
    );

    // The partial file has been read and named, so the directory this case worked in is
    // removed: a run of this test must not leave a .shellune-part file of its own in the
    // account's home for the next one.
    shell(server, &format!("rm -rf '{directory}'")).expect("the directory can be removed");
    assert!(
        !server_is_directory(server, &directory),
        "the interrupted run cleans up after itself"
    );
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

/// Reads a remote file back through a download and returns the bytes.
fn read_back(
    link: &mut TransferLink,
    recorder: &mut Recorder,
    remote: &str,
    name: &str,
) -> Vec<u8> {
    let directory = scratch_directory(name);
    let back = directory.join("back.bin");
    let id = start(
        link,
        TransferDirection::Download,
        remote,
        back.to_string_lossy().as_ref(),
    );
    match recorder.until_finished(link, id) {
        TransferUpdate::Completed { .. } => {}
        other => panic!("the read-back download did not complete: {other:?}"),
    }
    let bytes = std::fs::read(&back).expect("the downloaded file exists");
    let _ = std::fs::remove_dir_all(&directory);
    bytes
}

fn assert_refused(update: &TransferUpdate, reason: shellune_core::ResumeRefusal) {
    match update {
        TransferUpdate::Failed {
            failure: TransferFailure::ResumeRefused { reason: got },
            ..
        } => assert_eq!(*got, reason),
        other => panic!("the resume must be refused, not finished: {other:?}"),
    }
}

fn assert_no_fake_progress(recorder: &Recorder, id: TransferId) {
    assert!(
        recorder.updates(id).iter().all(|update| !matches!(
            update,
            TransferUpdate::Progress { .. }
                | TransferUpdate::Completed { .. }
                | TransferUpdate::Started
        )),
        "a refused resume writes nothing and reports no progress: {:?}",
        recorder.updates(id)
    );
}

#[test]
fn a_verified_prefix_resumes_from_its_offset_and_the_finished_file_matches() {
    if skip("resuming an upload") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let (mut session, mut link) = live_with_link(server);
    let (mut recorder, held) = hold_upload(server, &mut link, "resume", 16 * 1024 * 1024);
    let offset = held.hint.offset;

    let id = start_from(
        &mut link,
        TransferDirection::Upload,
        held.local.to_string_lossy().as_ref(),
        &held.remote,
        Some(held.hint.clone()),
    );
    match recorder.until_finished(&mut link, id) {
        TransferUpdate::Completed { bytes } => {
            assert_eq!(bytes, held.bytes.len() as u64);
        }
        other => panic!("the resumed upload did not complete: {other:?}"),
    }
    assert_eq!(
        first_progress(&recorder, id),
        Some(offset),
        "the first progress is the confirmed offset, not zero and not a guess"
    );
    assert!(
        recorder.updates(id).iter().all(|update| match update {
            TransferUpdate::Progress { transferred, .. } => *transferred >= offset,
            TransferUpdate::Completed { bytes } => *bytes == held.bytes.len() as u64,
            _ => true,
        }),
        "progress never goes backwards and completion is the whole file: {:?}",
        recorder.updates(id)
    );
    assert_eq!(
        server_size(server, &held.remote),
        Some(held.bytes.len() as u64)
    );
    assert!(
        !has_partial(&server_listing(server, &held.directory)),
        "the temporary file was renamed onto the target"
    );
    assert_eq!(
        read_back(&mut link, &mut recorder, &held.remote, "resume-back"),
        held.bytes,
        "the target holds the bytes that were uploaded"
    );
    assert!(
        !Path::new(&held.hint.record).exists(),
        "a finished resume removes its checkpoint"
    );
    session.close();
    let _ = shell(server, &format!("rm -rf '{}'", held.directory));
    let _ = std::fs::remove_dir_all(&held.scratch);
}

#[test]
fn a_changed_source_is_not_resumed_and_a_new_transfer_starts_over() {
    if skip("a changed source") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let (mut session, mut link) = live_with_link(server);
    let (mut recorder, held) = hold_upload(server, &mut link, "changed", 16 * 1024 * 1024);
    let mut changed = held.bytes.clone();
    changed.extend_from_slice(b"-changed");
    std::fs::write(&held.local, &changed).expect("the source can be replaced");

    let refused = start_from(
        &mut link,
        TransferDirection::Upload,
        held.local.to_string_lossy().as_ref(),
        &held.remote,
        Some(held.hint.clone()),
    );
    let update = recorder.until_finished(&mut link, refused);
    assert_refused(&update, shellune_core::ResumeRefusal::SourceChanged);
    assert_no_fake_progress(&recorder, refused);
    assert!(server_size(server, &held.remote).is_none());
    assert!(
        server_size(server, &held.partial).is_none(),
        "a prefix that no longer matches the source is not kept as a resume"
    );

    let fresh = start(
        &mut link,
        TransferDirection::Upload,
        held.local.to_string_lossy().as_ref(),
        &held.remote,
    );
    match recorder.until_finished(&mut link, fresh) {
        TransferUpdate::Completed { bytes } => assert_eq!(bytes, changed.len() as u64),
        other => panic!("the new transfer did not complete: {other:?}"),
    }
    assert!(
        matches!(
            recorder.updates(fresh).first().copied(),
            Some(TransferUpdate::Started)
        ),
        "a transfer that cannot resume starts from the beginning: {:?}",
        recorder.updates(fresh)
    );
    assert_ne!(fresh, refused);
    assert_eq!(
        read_back(&mut link, &mut recorder, &held.remote, "changed-back"),
        changed
    );
    session.close();
    let _ = std::fs::remove_file(&held.hint.record);
    let _ = shell(server, &format!("rm -rf '{}'", held.directory));
    let _ = std::fs::remove_dir_all(&held.scratch);
}

#[test]
fn an_unverifiable_or_damaged_partial_is_transferred_again_from_the_start() {
    if skip("an unverifiable partial") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let (mut session, mut link) = live_with_link(server);
    let (mut recorder, held) = hold_upload(server, &mut link, "unverifiable", 8 * 1024 * 1024);
    std::fs::remove_file(&held.hint.record).expect("the checkpoint file can be removed");

    let missing = start_from(
        &mut link,
        TransferDirection::Upload,
        held.local.to_string_lossy().as_ref(),
        &held.remote,
        Some(held.hint.clone()),
    );
    let update = recorder.until_finished(&mut link, missing);
    assert_refused(&update, shellune_core::ResumeRefusal::Unverifiable);
    assert_no_fake_progress(&recorder, missing);
    assert!(server_size(server, &held.remote).is_none());
    assert!(
        server_size(server, &held.partial).is_some(),
        "a checkpoint that cannot be read leaves the partial file in place"
    );
    let _ = shell(server, &format!("rm -rf '{}'", held.directory));
    let _ = std::fs::remove_dir_all(&held.scratch);

    // A damaged prefix is a different refusal, and it is not resumed either.
    let (mut recorder, held) = hold_upload(server, &mut link, "damaged", 8 * 1024 * 1024);
    // The sample's first byte is already zero, so the change has to be a different byte.
    shell(
        server,
        &format!(
            "printf '\\377' | dd of='{}' bs=1 count=1 conv=notrunc status=none",
            held.partial
        ),
    )
    .expect("the partial file can be altered");
    let damaged = start_from(
        &mut link,
        TransferDirection::Upload,
        held.local.to_string_lossy().as_ref(),
        &held.remote,
        Some(held.hint.clone()),
    );
    let update = recorder.until_finished(&mut link, damaged);
    assert_refused(&update, shellune_core::ResumeRefusal::PartialDamaged);
    assert_no_fake_progress(&recorder, damaged);
    assert!(server_size(server, &held.remote).is_none());

    let fresh = start(
        &mut link,
        TransferDirection::Upload,
        held.local.to_string_lossy().as_ref(),
        &held.remote,
    );
    match recorder.until_finished(&mut link, fresh) {
        TransferUpdate::Completed { bytes } => assert_eq!(bytes, held.bytes.len() as u64),
        other => panic!("the replacement transfer did not complete: {other:?}"),
    }
    assert!(
        matches!(
            recorder.updates(fresh).first().copied(),
            Some(TransferUpdate::Started)
        ),
        "the replacement transfer starts from the first byte: {:?}",
        recorder.updates(fresh)
    );
    assert_eq!(
        read_back(&mut link, &mut recorder, &held.remote, "damaged-back"),
        held.bytes
    );
    assert_ne!(fresh, damaged);
    session.close();
    let _ = shell(server, &format!("rm -rf '{}'", held.directory));
    let _ = std::fs::remove_dir_all(&held.scratch);
}

#[test]
fn a_verified_download_resumes_from_its_offset_and_matches_the_server() {
    if skip("resuming a download") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let directory = own_directory(server, "down-resume");
    let bytes = sample(8 * 1024 * 1024);
    let scratch = scratch_directory("down-resume-src");
    let local = local_file(&scratch, "source.bin", &bytes);
    let remote = format!("{directory}/source.bin");

    let (mut session, mut link) = live_with_link(server);
    let mut recorder = Recorder::default();
    let seed = start(
        &mut link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &remote,
    );
    assert!(matches!(
        recorder.until_finished(&mut link, seed),
        TransferUpdate::Completed { .. }
    ));

    let target_dir = scratch_directory("down-resume");
    let target = target_dir.join("source.bin");
    let id = start(
        &mut link,
        TransferDirection::Download,
        &remote,
        target.to_string_lossy().as_ref(),
    );
    recorder.until_partial_checkpoint(&mut link, id, bytes.len() as u64);
    cancel(&mut link, id);
    let hint = match recorder.until_finished(&mut link, id) {
        TransferUpdate::Cancelled { residue, resume } => {
            assert!(
                !target.exists(),
                "a cancelled download does not create the target"
            );
            let hint = resume.expect("the confirmed prefix is kept");
            let partial = residue.expect("the local partial file is named");
            let size = std::fs::metadata(&partial)
                .expect("the partial file exists")
                .len();
            assert!(size >= hint.offset && size < bytes.len() as u64);
            assert!(hint.offset > 0);
            hint
        }
        other => panic!("the cancelled download must end cancelled: {other:?}"),
    };

    let resumed = start_from(
        &mut link,
        TransferDirection::Download,
        &remote,
        target.to_string_lossy().as_ref(),
        Some(hint.clone()),
    );
    match recorder.until_finished(&mut link, resumed) {
        TransferUpdate::Completed { bytes: moved } => assert_eq!(moved, bytes.len() as u64),
        other => panic!("the resumed download did not complete: {other:?}"),
    }
    assert_eq!(first_progress(&recorder, resumed), Some(hint.offset));
    assert_eq!(std::fs::read(&target).expect("the file was written"), bytes);
    let left: Vec<String> = target_dir
        .read_dir()
        .expect("the folder is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !left.iter().any(|name| name.contains(".shellune-part-")),
        "the temporary file was renamed onto the target: {left:?}"
    );

    session.close();
    let _ = shell(server, &format!("rm -rf '{directory}'"));
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(&target_dir);
}

#[test]
fn closing_one_connection_does_not_stop_the_other_transfers() {
    if skip("two connections") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let first_dir = own_directory(server, "close-one");
    let second_dir = own_directory(server, "close-two");
    let bytes = sample(16 * 1024 * 1024);
    let scratch = scratch_directory("close-two-sessions");
    let local = local_file(&scratch, "shared.bin", &bytes);
    let first_remote = format!("{first_dir}/one.bin");
    let second_remote = format!("{second_dir}/two.bin");

    let (mut first_session, mut first_link) = live_with_link(server);
    let (mut second_session, mut second_link) = live_with_link(server);
    let mut first_recorder = Recorder::default();
    let mut second_recorder = Recorder::default();
    let first_id = start(
        &mut first_link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &first_remote,
    );
    let second_id = start(
        &mut second_link,
        TransferDirection::Upload,
        local.to_string_lossy().as_ref(),
        &second_remote,
    );
    first_recorder.until_progress(&mut first_link, first_id);
    second_recorder.until_progress(&mut second_link, second_id);

    first_session.close();
    let wait_until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < wait_until {
        first_recorder.collect(&mut first_link);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !first_recorder
            .updates(first_id)
            .iter()
            .any(|update| matches!(update, TransferUpdate::Completed { .. })),
        "closing the first connection does not complete its transfer: {:?}",
        first_recorder.updates(first_id)
    );
    if let Some(size) = server_size(server, &first_remote) {
        assert_ne!(size, bytes.len() as u64);
    }

    match second_recorder.until_finished(&mut second_link, second_id) {
        TransferUpdate::Completed { bytes: moved } => assert_eq!(moved, bytes.len() as u64),
        other => panic!("the second connection's transfer must finish: {other:?}"),
    }
    assert!(
        second_recorder
            .events
            .iter()
            .all(|event| event.transfer == second_id),
        "the second connection only reports its own task"
    );
    assert_eq!(
        server_size(server, &second_remote),
        Some(bytes.len() as u64)
    );
    assert_eq!(
        read_back(
            &mut second_link,
            &mut second_recorder,
            &second_remote,
            "close-two-back"
        ),
        bytes
    );
    second_session.close();
    let _ = shell(server, &format!("rm -rf '{first_dir}' '{second_dir}'"));
    let _ = std::fs::remove_dir_all(&scratch);
}
