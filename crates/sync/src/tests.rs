//! Sync between "machines" (separate data directories) through the in-process
//! WebDAV server.
//!
//! Every machine keeps its passwords in its own [`MemorySecrets`], so the
//! credential store of the computer running the tests is never touched.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use shellune_core::{
    AuthMethodKind, ConnectionRecord, HostConfig, HostId, SecretString, SerialConfig,
};
use shellune_storage::{
    AppSecret, ForwardStore, GroupStore, HostStore, KdfParams, KeymapEntry, KeymapPreferences,
    LocalData, LocalState, MemorySecrets, Platform, PlatformField, PortableError, PreferencesStore,
    SecretStore, ShelluneData, SnippetStore, TrustStore,
};

use crate::testing::WebDavServer;
use crate::{
    LastResult, REMOTE_FILE_NAME, Resolution, SyncConfig, SyncError, SyncOutcome, SyncService,
    WebDavError,
};

const USER: &str = "alice";
const DAV_PASSWORD: &str = "dav-password-4711";
const HOST_PASSWORD: &str = "host-password-correct-horse";
const PASSPHRASE: &str = "sync passphrase of mine";
const REMOTE_PATH: &str = "Shellune/shellune-data.json";

fn scratch(name: &str) -> PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shellune-sync-{name}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// One machine: its data directory, its credential store and its sync service.
struct Machine {
    root: PathBuf,
    secrets: Arc<MemorySecrets>,
    service: SyncService,
}

impl Machine {
    fn new(name: &str, platform: Platform) -> Self {
        let root = scratch(name);
        std::fs::create_dir_all(&root).expect("a data directory");
        let secrets = Arc::new(MemorySecrets::new());
        let store: Arc<dyn SecretStore> = secrets.clone();
        let service = SyncService::new(LocalData::at(&root), platform, store)
            .with_kdf(KdfParams::new(64, 1, 1).expect("cheap parameters"));
        Self {
            root,
            secrets,
            service,
        }
    }

    /// Points the machine at the server's `Shellune` folder.
    fn connect(&self, server: &WebDavServer) {
        self.service
            .save_config(&SyncConfig::new(server.url(), USER))
            .expect("settings saved");
        self.service
            .set_password(Some(&SecretString::new(DAV_PASSWORD)))
            .expect("password saved");
    }

    fn config(&self) -> SyncConfig {
        self.service
            .load_config()
            .expect("readable")
            .expect("configured")
    }

    fn update_config(&self, change: impl FnOnce(&mut SyncConfig)) {
        let mut config = self.config();
        change(&mut config);
        self.service.save_config(&config).expect("saved");
    }

    fn load(&self) -> LocalState {
        LocalData::at(&self.root).load().expect("local data loads")
    }

    fn save(&self, state: &LocalState) {
        save_state(&self.root, state);
    }

    fn edit(&self, change: impl FnOnce(&mut LocalState)) {
        let mut state = self.load();
        change(&mut state);
        self.save(&state);
    }

    fn host_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .load()
            .hosts
            .records()
            .iter()
            .map(|record| record.name().to_owned())
            .collect();
        names.sort();
        names
    }

    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        tree(&self.root)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn save_state(root: &Path, state: &LocalState) {
    HostStore::at(root).save(&state.hosts).expect("hosts");
    GroupStore::at(root).save(&state.groups).expect("groups");
    ForwardStore::at(root)
        .save(&state.forwards)
        .expect("forwards");
    SnippetStore::at(root)
        .save(&state.snippets)
        .expect("snippets");
    TrustStore::at(root)
        .save(&state.known_hosts)
        .expect("trust");
    PreferencesStore::at(root)
        .save(&state.preferences)
        .expect("preferences");
}

/// Every data file (snapshots, settings and state left out), for "nothing
/// changed" checks.
fn data_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    tree(root)
        .into_iter()
        .filter(|(path, _)| {
            let relative = path.strip_prefix(root).expect("inside");
            !relative.starts_with("snapshots")
                && relative != Path::new(crate::STATE_FILE_NAME)
                && relative != Path::new(crate::CONFIG_FILE_NAME)
        })
        .collect()
}

fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return files;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(tree(&path));
        } else {
            files.push((path.clone(), std::fs::read(&path).expect("readable")));
        }
    }
    files.sort();
    files
}

fn add_host(state: &mut LocalState, name: &str) -> HostId {
    let host = HostConfig::new(name, "10.0.0.1", 22, "deploy").expect("host");
    let id = host.id();
    state.hosts.insert(host).expect("inserted");
    id
}

fn remove_host(state: &mut LocalState, name: &str) {
    let id = state
        .hosts
        .records()
        .iter()
        .find(|record| record.name() == name)
        .map(ConnectionRecord::id)
        .expect("present");
    state.hosts.remove_connection(id).expect("removed");
}

fn remote_text(server: &WebDavServer) -> String {
    String::from_utf8(server.file(REMOTE_PATH).expect("a remote file")).expect("utf-8")
}

fn remote_document(server: &WebDavServer) -> ShelluneData {
    ShelluneData::from_json(&server.file(REMOTE_PATH).expect("a remote file")).expect("readable")
}

fn keymap_entry(keystroke: &str) -> KeymapEntry {
    KeymapEntry::new("palette.open", "shell", Some(keystroke.to_owned()))
}

#[track_caller]
fn expect_uploaded(outcome: Result<SyncOutcome, SyncError>) -> crate::UploadReport {
    match outcome {
        Ok(SyncOutcome::Uploaded(report)) => report,
        other => panic!("expected an upload, got {other:?}"),
    }
}

#[track_caller]
fn expect_pulled(outcome: Result<SyncOutcome, SyncError>) -> crate::PullReport {
    match outcome {
        Ok(SyncOutcome::Pulled(report)) => *report,
        other => panic!("expected a pull, got {other:?}"),
    }
}

#[track_caller]
fn expect_up_to_date(outcome: Result<SyncOutcome, SyncError>) {
    assert!(
        matches!(outcome, Ok(SyncOutcome::UpToDate)),
        "expected nothing to do, got {outcome:?}"
    );
}

#[track_caller]
fn expect_conflict(outcome: Result<SyncOutcome, SyncError>) -> crate::ConflictReport {
    match outcome {
        Ok(SyncOutcome::Conflict(report)) => report,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn testing_the_connection_makes_the_folder_and_tells_failures_apart() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let password = SecretString::new(DAV_PASSWORD);
    let mut config = SyncConfig::new(server.url(), USER);
    config.remote_dir = "Backups/Shellune".to_owned();

    SyncService::test_connection(&config, Some(&password)).expect("the test passes");
    assert!(server.has_folder("Backups"), "{:?}", server.requests());
    assert!(server.has_folder("Backups/Shellune"));
    assert!(server.files().is_empty(), "the probe file was removed");
    // The folder exists now; testing again passes without making it.
    SyncService::test_connection(&config, Some(&password)).expect("the test passes");

    let wrong = SecretString::new("not the password");
    assert_eq!(
        SyncService::test_connection(&config, Some(&wrong)),
        Err(WebDavError::Auth { status: 401 })
    );

    server.set_read_only(true);
    assert!(matches!(
        SyncService::test_connection(&config, Some(&password)),
        Err(WebDavError::NotWritable { status: 403, .. })
    ));
    let mut missing = config.clone();
    missing.remote_dir = "Elsewhere".to_owned();
    let error = SyncService::test_connection(&missing, Some(&password)).expect_err("refused");
    assert!(
        matches!(&error, WebDavError::NotWritable { path, .. } if path == "Elsewhere"),
        "{error:?}"
    );
    server.set_read_only(false);

    server.set_offline(true);
    assert!(matches!(
        SyncService::test_connection(&config, Some(&password)),
        Err(WebDavError::Unreachable { .. })
    ));
    server.set_offline(false);

    // A port nothing listens on.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = closed.local_addr().expect("an address");
    drop(closed);
    let nowhere = SyncConfig::new(format!("http://{address}/dav/"), USER);
    assert!(matches!(
        SyncService::test_connection(&nowhere, Some(&password)),
        Err(WebDavError::Unreachable { .. })
    ));
}

#[test]
fn nothing_is_sent_until_a_server_is_set() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let machine = Machine::new("unconfigured", Platform::Linux);
    machine.edit(|state| {
        add_host(state, "web");
    });
    assert_eq!(
        machine.service.sync().unwrap_err(),
        SyncError::NotConfigured
    );
    assert_eq!(
        machine.service.upload().unwrap_err(),
        SyncError::NotConfigured
    );
    machine
        .service
        .save_config(&SyncConfig::default())
        .expect("saved");
    assert_eq!(
        machine.service.pull().unwrap_err(),
        SyncError::NotConfigured
    );
    assert!(server.requests().is_empty());
    assert_eq!(machine.service.state(), crate::SyncState::default());
}

#[test]
fn the_settings_file_holds_no_password() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let machine = Machine::new("settings", Platform::Windows);
    machine.connect(&server);
    machine
        .service
        .set_passphrase(Some(&SecretString::new(PASSPHRASE)))
        .expect("saved");
    let config = machine.config();
    assert!(config.auto, "automatic sync is on by default");
    assert!(!config.sync_secrets, "passwords are not synced by default");
    let text = std::fs::read_to_string(machine.root.join(crate::CONFIG_FILE_NAME)).expect("file");
    assert!(!text.contains(DAV_PASSWORD), "{text}");
    assert!(!text.contains(PASSPHRASE), "{text}");
    assert_eq!(
        machine
            .secrets
            .load_app(AppSecret::WebDavPassword)
            .expect("read")
            .map(|secret| secret.expose().to_owned()),
        Some(DAV_PASSWORD.to_owned())
    );
    assert_eq!(
        machine
            .service
            .passphrase()
            .expect("read")
            .map(|secret| secret.expose().to_owned()),
        Some(PASSPHRASE.to_owned())
    );
    machine.service.set_password(None).expect("forgotten");
    assert!(machine.service.password().expect("read").is_none());
    // Without the password the server refuses, and the reason is recorded.
    assert_eq!(
        machine.service.sync().unwrap_err(),
        SyncError::Remote(WebDavError::Auth { status: 401 })
    );
    assert!(matches!(
        machine.service.state().last_result,
        Some(LastResult::Failed { .. })
    ));
}

#[test]
fn two_machines_keep_each_other_up_to_date() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("two-a", Platform::Linux);
    let b = Machine::new("two-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);

    a.edit(|state| {
        add_host(state, "web");
        add_host(state, "db");
        state.snippets.create("disk", "df -h").expect("snippet");
        state.preferences.theme = Some("dark".to_owned());
    });
    // First sync with no file in the cloud: upload.
    let report = expect_uploaded(a.service.sync());
    assert!(report.created);
    assert!(server.has_folder("Shellune"));
    let text = remote_text(&server);
    assert!(!text.contains("\"secrets\""), "{text}");
    assert_eq!(
        a.service.state().last_result,
        Some(LastResult::Uploaded),
        "the result is recorded"
    );

    // Nothing changed: only the version is looked up.
    let before = server.requests().len();
    expect_up_to_date(a.service.sync());
    let asked: Vec<String> = server.requests()[before..].to_vec();
    assert!(
        asked.iter().all(|request| request.starts_with("PROPFIND")),
        "{asked:?}"
    );

    // An empty machine pulls on its first sync.
    let pulled = expect_pulled(b.service.sync());
    assert_eq!(pulled.hosts.added, 2);
    assert_eq!(pulled.snippets.added, 1);
    assert_eq!(b.host_names(), vec!["db", "web"]);
    assert_eq!(b.load().preferences.theme.as_deref(), Some("dark"));
    expect_up_to_date(b.service.sync());

    // A change on one side reaches the other, deletions included.
    b.edit(|state| {
        add_host(state, "cache");
        remove_host(state, "db");
    });
    let report = expect_uploaded(b.service.sync());
    assert!(!report.created);
    let pulled = expect_pulled(a.service.sync());
    assert_eq!(pulled.hosts.added, 1);
    assert_eq!(pulled.hosts.removed, 1);
    assert_eq!(a.host_names(), vec!["cache", "web"]);
    assert!(pulled.state.hosts.records().len() == 2);
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());
}

#[test]
fn keymaps_and_key_paths_stay_with_their_platform() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let windows = Machine::new("cross-windows", Platform::Windows);
    let linux = Machine::new("cross-linux", Platform::Linux);
    windows.connect(&server);
    linux.connect(&server);

    let windows_key = r"C:\Users\me\.ssh\db_ed25519";
    let linux_key = "/home/me/.ssh/db_ed25519";
    let mut db_id = None;
    let mut serial_id = None;
    windows.edit(|state| {
        let mut db = HostConfig::new("db", "10.0.0.6", 22, "postgres").expect("host");
        db.set_identity_file(Some(windows_key));
        db_id = Some(db.id());
        state.hosts.insert(db).expect("inserted");
        let serial = SerialConfig::new("console", "COM3").expect("serial");
        serial_id = Some(serial.id());
        state
            .hosts
            .insert_connection(ConnectionRecord::Serial(serial))
            .expect("inserted");
        let mut keymap = KeymapPreferences::default();
        keymap.set_for_platform("windows", vec![keymap_entry("ctrl-alt-p")]);
        state.preferences.keymap = Some(keymap);
        state.preferences.text_size = Some(125);
    });
    let (db_id, serial_id) = (db_id.expect("db"), serial_id.expect("serial"));
    expect_uploaded(windows.service.sync());

    // Linux has no section in the cloud yet: the Windows values are kept and
    // reported, and nothing of the Windows key bindings becomes Linux's.
    let pulled = expect_pulled(linux.service.sync());
    let reported: Vec<(HostId, PlatformField)> = pulled
        .fallbacks
        .iter()
        .map(|fallback| (fallback.id, fallback.field))
        .collect();
    assert!(reported.contains(&(db_id, PlatformField::IdentityFile)));
    assert!(reported.contains(&(serial_id, PlatformField::SerialDevice)));
    let state = linux.load();
    let keymap = state.preferences.keymap.clone().unwrap_or_default();
    assert!(keymap.for_platform("linux").is_empty());
    assert_eq!(state.preferences.text_size, None);

    // Linux sets its own path, device and binding, and uploads.
    linux.edit(|state| {
        let ConnectionRecord::Ssh(mut db) = state.hosts.connection(db_id).expect("db") else {
            panic!("an SSH host");
        };
        db.set_identity_file(Some(linux_key));
        state.hosts.update(db, db_id).expect("updated");
        let ConnectionRecord::Serial(mut serial) =
            state.hosts.connection(serial_id).expect("serial")
        else {
            panic!("a serial connection");
        };
        serial.set_device("/dev/ttyUSB0").expect("device");
        state
            .hosts
            .update_connection(ConnectionRecord::Serial(serial), serial_id)
            .expect("updated");
        let mut keymap = state.preferences.keymap.clone().unwrap_or_default();
        keymap.set_for_platform("linux", vec![keymap_entry("ctrl-shift-p")]);
        state.preferences.keymap = Some(keymap);
    });
    expect_uploaded(linux.service.sync());

    // The cloud file carries both sections.
    let document = remote_document(&server);
    let windows_section = document.platform(Platform::Windows).expect("windows");
    let linux_section = document.platform(Platform::Linux).expect("linux");
    assert_eq!(
        windows_section.hosts[&db_id].identity_file.as_deref(),
        Some(windows_key)
    );
    assert_eq!(
        linux_section.hosts[&db_id].identity_file.as_deref(),
        Some(linux_key)
    );
    assert_eq!(
        windows_section.keymap,
        Some(vec![keymap_entry("ctrl-alt-p")])
    );
    assert_eq!(
        linux_section.keymap,
        Some(vec![keymap_entry("ctrl-shift-p")])
    );

    // Windows takes the change but keeps its own path, device and bindings.
    expect_pulled(windows.service.sync());
    let state = windows.load();
    let Some(ConnectionRecord::Ssh(db)) = state.hosts.connection(db_id) else {
        panic!("an SSH host");
    };
    assert_eq!(db.identity_file(), Some(windows_key));
    let Some(ConnectionRecord::Serial(serial)) = state.hosts.connection(serial_id) else {
        panic!("a serial connection");
    };
    assert_eq!(serial.device(), "COM3");
    let keymap = state.preferences.keymap.clone().expect("bindings");
    assert_eq!(
        keymap.for_platform("windows"),
        &[keymap_entry("ctrl-alt-p")]
    );
    assert_eq!(state.preferences.text_size, Some(125));

    // The two views differ by design, which is not a change on either side.
    expect_up_to_date(windows.service.sync());
    expect_up_to_date(linux.service.sync());
    expect_up_to_date(windows.service.sync());

    // A Windows change keeps the Linux section in the cloud.
    windows.edit(|state| {
        add_host(state, "new-on-windows");
    });
    expect_uploaded(windows.service.sync());
    let document = remote_document(&server);
    assert_eq!(
        document.platform(Platform::Linux).expect("linux").hosts[&db_id]
            .identity_file
            .as_deref(),
        Some(linux_key)
    );
    expect_pulled(linux.service.sync());
    let state = linux.load();
    let Some(ConnectionRecord::Ssh(db)) = state.hosts.connection(db_id) else {
        panic!("an SSH host");
    };
    assert_eq!(db.identity_file(), Some(linux_key));
}

#[test]
fn a_conflict_waits_for_the_user_and_each_answer_does_what_it_says() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("conflict-a", Platform::Linux);
    let b = Machine::new("conflict-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);
    a.edit(|state| {
        add_host(state, "shared");
    });
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());

    // Both change.
    a.edit(|state| {
        add_host(state, "a-only");
    });
    expect_uploaded(a.service.sync());
    b.edit(|state| {
        add_host(state, "b-only");
    });
    let b_files = data_files(&b.root);
    let remote_before = server.file(REMOTE_PATH).expect("remote");
    let report = expect_conflict(b.service.sync());
    assert!(!report.first_sync);
    assert_eq!(report.remote_platform, "linux");
    assert_eq!(data_files(&b.root), b_files, "nothing changed here");
    assert_eq!(server.file(REMOTE_PATH).expect("remote"), remote_before);
    assert!(b.service.state().has_conflict());

    // Cancel: nothing changes and the conflict stays.
    assert!(matches!(
        b.service.resolve(Resolution::Cancel),
        Ok(SyncOutcome::ConflictKept)
    ));
    assert_eq!(data_files(&b.root), b_files);
    assert_eq!(server.file(REMOTE_PATH).expect("remote"), remote_before);
    assert!(b.service.state().has_conflict());
    expect_conflict(b.service.sync());

    // Use local: the cloud file is kept here, then replaced by this machine's data.
    let report = expect_uploaded(b.service.resolve(Resolution::UseLocal));
    let kept = report.remote_snapshot.expect("the cloud file is kept");
    assert!(kept.starts_with(b.root.join("snapshots")));
    assert_eq!(
        std::fs::read(kept.join(REMOTE_FILE_NAME)).expect("kept"),
        remote_before
    );
    assert_eq!(data_files(&b.root), b_files, "local data is untouched");
    let names: Vec<String> = remote_document(&server)
        .shared()
        .hosts()
        .records()
        .iter()
        .map(|record| record.name().to_owned())
        .collect();
    assert!(names.contains(&"b-only".to_owned()));
    assert!(!names.contains(&"a-only".to_owned()));
    assert!(!b.service.state().has_conflict());
    expect_up_to_date(b.service.sync());

    // A changed meanwhile too: a conflict on its side; use the cloud's data.
    a.edit(|state| {
        add_host(state, "a-two");
    });
    let a_hosts_before = std::fs::read(a.root.join("hosts.json")).expect("hosts");
    expect_conflict(a.service.sync());
    let pulled = expect_pulled(a.service.resolve(Resolution::UseRemote));
    assert_eq!(a.host_names(), vec!["b-only", "shared"]);
    let snapshot = pulled.snapshot.expect("the local data is kept");
    assert_eq!(
        std::fs::read(snapshot.join("hosts.json")).expect("kept"),
        a_hosts_before,
        "the replaced local data can be recovered"
    );
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());
}

#[test]
fn a_first_sync_with_different_data_on_both_sides_is_a_conflict() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("first-a", Platform::Linux);
    let b = Machine::new("first-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);
    a.edit(|state| {
        add_host(state, "from-a");
    });
    b.edit(|state| {
        add_host(state, "from-b");
    });
    expect_uploaded(a.service.sync());
    let report = expect_conflict(b.service.sync());
    assert!(report.first_sync);
    assert_eq!(b.host_names(), vec!["from-b"]);

    // Equal data on both sides is only recorded.
    let c = Machine::new("first-c", Platform::Linux);
    c.connect(&server);
    std::fs::copy(a.root.join("hosts.json"), c.root.join("hosts.json")).expect("copied");
    expect_up_to_date(c.service.sync());
    assert!(c.service.state().last_synced_hash.is_some());
    expect_up_to_date(c.service.sync());

    // An empty machine with automatic sync uploads nothing.
    let empty = Machine::new("first-empty", Platform::Linux);
    let other = WebDavServer::start(USER, DAV_PASSWORD);
    empty.connect(&other);
    expect_up_to_date(empty.service.sync());
    assert!(other.files().is_empty());
    assert_eq!(empty.service.pull().unwrap_err(), SyncError::NothingInCloud);
}

#[test]
fn upload_and_pull_only_go_one_way() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("oneway-a", Platform::Linux);
    let b = Machine::new("oneway-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);
    a.update_config(|config| config.auto = false);
    a.edit(|state| {
        add_host(state, "one");
    });
    expect_uploaded(a.service.upload());
    expect_pulled(b.service.pull());

    // Only the cloud changed: "upload now" does not overwrite it.
    b.edit(|state| {
        add_host(state, "two");
    });
    expect_uploaded(b.service.upload());
    expect_conflict(a.service.upload());
    expect_pulled(a.service.pull());
    assert_eq!(a.host_names(), vec!["one", "two"]);

    // Only this machine changed: "pull" does not throw the change away.
    a.edit(|state| {
        add_host(state, "three");
    });
    expect_conflict(a.service.pull());
    assert_eq!(a.host_names(), vec!["one", "three", "two"]);
    expect_uploaded(a.service.upload());
    expect_pulled(b.service.sync());
    assert_eq!(b.host_names(), vec!["one", "three", "two"]);
}

#[test]
fn passwords_travel_only_when_asked_and_only_sealed() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("secrets-a", Platform::Windows);
    let b = Machine::new("secrets-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);
    let mut web_id = None;
    a.edit(|state| {
        let mut web = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("host");
        web.set_auth(Some(AuthMethodKind::Password));
        web.set_remember_secret(true);
        web_id = Some(web.id());
        state.hosts.insert(web).expect("inserted");
    });
    let web_id = web_id.expect("web");
    a.secrets
        .save(web_id, &SecretString::new(HOST_PASSWORD))
        .expect("saved");

    // Off: no secrets field and no password anywhere in the file.
    expect_uploaded(a.service.sync());
    let text = remote_text(&server);
    assert!(!text.contains("\"secrets\""), "{text}");
    assert!(!text.contains(HOST_PASSWORD));

    // On without a passphrase: refused, nothing uploaded.
    a.update_config(|config| config.sync_secrets = true);
    assert_eq!(a.service.sync().unwrap_err(), SyncError::PassphraseMissing);
    assert_eq!(remote_text(&server), text);

    // On with a passphrase: the passwords go up sealed.
    a.service
        .set_passphrase(Some(&SecretString::new(PASSPHRASE)))
        .expect("saved");
    let report = expect_uploaded(a.service.sync());
    assert_eq!(report.secrets_included, vec![web_id]);
    let text = remote_text(&server);
    assert!(text.contains("\"secrets\""), "{text}");
    assert!(!text.contains(HOST_PASSWORD));
    expect_up_to_date(a.service.sync());

    // A machine that does not sync passwords takes the records, not the passwords.
    let pulled = expect_pulled(b.service.sync());
    assert!(pulled.secrets_ignored);
    assert_eq!(pulled.secrets_saved, 0);
    assert!(b.secrets.is_empty());

    // Switching it on with the wrong passphrase fails and changes nothing.
    b.update_config(|config| config.sync_secrets = true);
    b.service
        .set_passphrase(Some(&SecretString::new("another passphrase")))
        .expect("saved");
    let b_files = data_files(&b.root);
    assert_eq!(
        b.service.sync().unwrap_err(),
        SyncError::RemoteDocument(PortableError::WrongPassphrase)
    );
    assert_eq!(data_files(&b.root), b_files);
    assert!(b.secrets.is_empty());
    assert_eq!(remote_text(&server), text, "nothing was uploaded over them");

    // With the right one the cloud's passwords are taken, not overwritten.
    b.service
        .set_passphrase(Some(&SecretString::new(PASSPHRASE)))
        .expect("saved");
    let pulled = expect_pulled(b.service.sync());
    assert_eq!(pulled.secrets_saved, 1);
    assert_eq!(
        b.secrets
            .load(web_id)
            .expect("read")
            .map(|secret| secret.expose().to_owned()),
        Some(HOST_PASSWORD.to_owned())
    );
    assert!(
        b.load()
            .hosts
            .connection(web_id)
            .is_some_and(|record| record.remembers_secret())
    );
    expect_up_to_date(b.service.sync());

    // A changed password is a change.
    a.secrets
        .save(web_id, &SecretString::new("a-new-password"))
        .expect("saved");
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());
    assert_eq!(
        b.secrets
            .load(web_id)
            .expect("read")
            .map(|secret| secret.expose().to_owned()),
        Some("a-new-password".to_owned())
    );

    // Switching it off removes the passwords from the cloud.
    a.update_config(|config| config.sync_secrets = false);
    expect_uploaded(a.service.sync());
    let text = remote_text(&server);
    assert!(!text.contains("\"secrets\""), "{text}");
    let state_text =
        std::fs::read_to_string(b.root.join(crate::STATE_FILE_NAME)).expect("state file");
    assert!(!state_text.contains(HOST_PASSWORD));
    assert!(!state_text.contains("a-new-password"));
}

#[test]
fn a_failure_keeps_local_data_and_the_next_sync_recovers() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("offline-a", Platform::Linux);
    let b = Machine::new("offline-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);
    a.edit(|state| {
        add_host(state, "first");
    });
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());

    // Offline: the change stays here and the failure is recorded.
    server.set_offline(true);
    a.edit(|state| {
        add_host(state, "while-offline");
    });
    let files = data_files(&a.root);
    let error = a.service.sync().expect_err("offline");
    assert!(
        matches!(error, SyncError::Remote(WebDavError::Unreachable { .. })),
        "{error:?}"
    );
    assert_eq!(data_files(&a.root), files);
    let state = a.service.state();
    assert!(matches!(
        state.last_result,
        Some(LastResult::Failed { ref reason }) if reason.contains("reached")
    ));
    // A pull while offline does not touch the other machine either.
    let b_files = data_files(&b.root);
    assert!(b.service.sync().is_err());
    assert_eq!(data_files(&b.root), b_files);

    // Back online: the next sync uploads the change made while offline.
    server.set_offline(false);
    expect_uploaded(a.service.sync());
    assert_eq!(a.service.state().last_result, Some(LastResult::Uploaded));
    expect_pulled(b.service.sync());
    assert_eq!(b.host_names(), vec!["first", "while-offline"]);

    // A server that refuses writes fails the same way and loses nothing.
    server.set_read_only(true);
    a.edit(|state| {
        add_host(state, "read-only");
    });
    let error = a.service.sync().expect_err("refused");
    assert!(
        matches!(error, SyncError::Remote(WebDavError::NotWritable { .. })),
        "{error:?}"
    );
    server.set_read_only(false);
    expect_uploaded(a.service.sync());
}

#[test]
fn a_write_refused_because_the_file_changed_is_decided_again() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("race-a", Platform::Linux);
    a.connect(&server);
    a.edit(|state| {
        add_host(state, "one");
    });
    expect_uploaded(a.service.sync());
    a.edit(|state| {
        add_host(state, "two");
    });
    let before = server.requests().len();
    server.fail_next_put();
    expect_uploaded(a.service.sync());
    let puts = server.requests()[before..]
        .iter()
        .filter(|request| request.starts_with("PUT"))
        .count();
    assert_eq!(
        puts, 2,
        "the refused write and the one after deciding again"
    );
    expect_up_to_date(a.service.sync());

    // A real change by another client between the look-up and the write is not
    // overwritten: the stale tag is refused.
    let b = Machine::new("race-b", Platform::Linux);
    b.connect(&server);
    expect_pulled(b.service.sync());
    b.edit(|state| {
        add_host(state, "three");
    });
    expect_uploaded(b.service.sync());
    let client = crate::WebDavClient::new(
        &server.url(),
        "Shellune",
        USER,
        Some(&SecretString::new(DAV_PASSWORD)),
    )
    .expect("client");
    let stale = a.service.state().last_remote_etag.expect("a tag");
    assert_eq!(
        client.put(b"{}", &crate::Precondition::IfMatch(stale)),
        Err(WebDavError::Conflict)
    );
    assert_eq!(
        client.put(b"{}", &crate::Precondition::IfAbsent),
        Err(WebDavError::Conflict)
    );
}

#[test]
fn a_server_without_entity_tags_still_syncs() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    server.set_etags(false);
    let a = Machine::new("no-tags-a", Platform::Linux);
    let b = Machine::new("no-tags-b", Platform::Linux);
    a.connect(&server);
    b.connect(&server);
    a.edit(|state| {
        add_host(state, "one");
    });
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());
    b.edit(|state| {
        add_host(state, "two");
    });
    expect_uploaded(b.service.sync());
    expect_pulled(a.service.sync());
    assert_eq!(a.host_names(), vec!["one", "two"]);
}

#[test]
fn a_damaged_or_newer_cloud_file_is_never_applied_or_overwritten() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("damaged", Platform::Linux);
    a.connect(&server);
    a.edit(|state| {
        add_host(state, "mine");
    });
    expect_uploaded(a.service.sync());

    let newer = remote_text(&server).replacen("\"version\": 1", "\"version\": 99", 1);
    server.put_file(REMOTE_PATH, newer.as_bytes());
    a.edit(|state| {
        add_host(state, "more");
    });
    let files = data_files(&a.root);
    assert!(matches!(
        a.service.sync().unwrap_err(),
        SyncError::RemoteDocument(PortableError::UnsupportedVersion { found: 99, .. })
    ));
    assert_eq!(data_files(&a.root), files);
    assert_eq!(
        remote_text(&server),
        newer,
        "a newer file is not overwritten"
    );

    server.put_file(REMOTE_PATH, b"{ not json");
    assert!(matches!(
        a.service.sync().unwrap_err(),
        SyncError::RemoteDocument(PortableError::NotShelluneData { .. })
    ));
    assert_eq!(data_files(&a.root), files);

    // The user may still choose this machine's data; the damaged file is kept.
    let report = expect_uploaded(a.service.resolve(Resolution::UseLocal));
    let kept = report.remote_snapshot.expect("kept");
    assert_eq!(
        std::fs::read(kept.join(REMOTE_FILE_NAME)).expect("kept"),
        b"{ not json"
    );
    assert_eq!(
        remote_document(&server).shared().hosts().connection_count(),
        2
    );
    assert!(!a.files().is_empty());
}

// ---------------------------------------------------------------------------
// Merging when one side only connected to hosts
// ---------------------------------------------------------------------------

#[track_caller]
fn expect_merged(outcome: Result<SyncOutcome, SyncError>) -> crate::PullReport {
    match outcome {
        Ok(SyncOutcome::Merged(report)) => *report,
        other => panic!("expected a merge, got {other:?}"),
    }
}

fn id_of(state: &LocalState, name: &str) -> HostId {
    state
        .hosts
        .records()
        .iter()
        .find(|record| record.name() == name)
        .map(ConnectionRecord::id)
        .expect("present")
}

/// Records a connection to `name` at `at`, the way the application does after a
/// session opens.
fn connect_to(state: &mut LocalState, name: &str, at: u64) {
    let id = id_of(state, name);
    state.hosts.record_connection(id, at).expect("recorded");
}

/// Changes the address of `name`: a change of the records, not of the recents.
fn readdress(state: &mut LocalState, name: &str, address: &str) {
    let id = id_of(state, name);
    let ConnectionRecord::Ssh(mut host) = state.hosts.connection(id).expect("present") else {
        panic!("an SSH host");
    };
    host.set_address(address).expect("address");
    state.hosts.update(host, id).expect("updated");
}

fn address_of(machine: &Machine, name: &str) -> String {
    let state = machine.load();
    let id = id_of(&state, name);
    match state.hosts.connection(id).expect("present") {
        ConnectionRecord::Ssh(host) => host.address().to_owned(),
        other => panic!("an SSH host, got {other:?}"),
    }
}

/// When `name` was last reached and how often, as `book` remembers it.
fn recent_in(book: &shellune_storage::HostBook, name: &str) -> Option<(u64, u32)> {
    let id = book
        .records()
        .iter()
        .find(|record| record.name() == name)
        .map(ConnectionRecord::id)?;
    book.recent_of(id)
        .map(|recent| (recent.last_connected_unix(), recent.connections()))
}

fn recent_of(machine: &Machine, name: &str) -> Option<(u64, u32)> {
    recent_in(&machine.load().hosts, name)
}

fn remote_recent(server: &WebDavServer, name: &str) -> Option<(u64, u32)> {
    recent_in(remote_document(server).shared().hosts(), name)
}

fn puts_since(server: &WebDavServer, before: usize) -> usize {
    server.requests()[before..]
        .iter()
        .filter(|request| request.starts_with("PUT"))
        .count()
}

/// Two machines that synced `web` and `db`.
fn synced_pair(name: &str, server: &WebDavServer) -> (Machine, Machine) {
    let a = Machine::new(&format!("{name}-a"), Platform::Linux);
    let b = Machine::new(&format!("{name}-b"), Platform::Linux);
    a.connect(server);
    b.connect(server);
    a.edit(|state| {
        add_host(state, "web");
        add_host(state, "db");
    });
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());
    (a, b)
}

#[test]
fn connecting_here_while_the_cloud_changed_records_merges_without_a_conflict() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-remote", &server);

    // Scenario one: A changes `web` and uploads; B only connected to `db`.
    a.edit(|state| readdress(state, "web", "10.9.9.9"));
    expect_uploaded(a.service.sync());
    b.edit(|state| connect_to(state, "db", 1_700_000_500));
    let b_hosts_before = std::fs::read(b.root.join("hosts.json")).expect("hosts");
    let report = expect_merged(b.service.sync());
    assert_eq!(address_of(&b, "web"), "10.9.9.9", "A's change arrived");
    assert_eq!(recent_of(&b, "db"), Some((1_700_000_500, 1)), "B's kept");
    assert_eq!(remote_recent(&server, "db"), Some((1_700_000_500, 1)));
    assert_eq!(report.hosts.updated, 1);
    assert_eq!(
        report.state,
        b.load(),
        "the report holds the data as written"
    );
    // The data replaced here is kept.
    let snapshot = report.snapshot.expect("a snapshot");
    assert!(
        snapshot.to_string_lossy().contains("sync-merge"),
        "{snapshot:?}"
    );
    assert_eq!(
        std::fs::read(snapshot.join("hosts.json")).expect("kept"),
        b_hosts_before
    );
    let state = b.service.state();
    assert!(!state.has_conflict());
    assert_eq!(state.last_result, Some(LastResult::UpToDate));
    expect_up_to_date(b.service.sync());
    // A sees B's recent connection.
    expect_pulled(a.service.sync());
    assert_eq!(recent_of(&a, "db"), Some((1_700_000_500, 1)));
    expect_up_to_date(a.service.sync());

    // Scenario three: A only connects; B changes `db` and uploads.
    a.edit(|state| connect_to(state, "web", 1_700_000_600));
    b.edit(|state| readdress(state, "db", "10.8.8.8"));
    expect_uploaded(b.service.sync());
    expect_merged(a.service.sync());
    assert_eq!(address_of(&a, "db"), "10.8.8.8");
    assert_eq!(recent_of(&a, "web"), Some((1_700_000_600, 1)));
    assert_eq!(remote_recent(&server, "web"), Some((1_700_000_600, 1)));
    expect_pulled(b.service.sync());
    // Both machines end with the same records and recents.
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());
}

#[test]
fn records_changed_here_keep_the_recents_connected_in_the_cloud() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-local", &server);

    // B only connects and uploads; A changes a record meanwhile.
    b.edit(|state| connect_to(state, "db", 1_700_000_700));
    expect_uploaded(b.service.sync());
    a.edit(|state| {
        readdress(state, "web", "10.7.7.7");
        connect_to(state, "web", 1_700_000_800);
    });
    let report = expect_merged(a.service.sync());
    assert!(
        report.snapshot.is_some(),
        "the recents written here are kept"
    );
    assert_eq!(address_of(&a, "web"), "10.7.7.7", "A's change stays");
    assert_eq!(recent_of(&a, "db"), Some((1_700_000_700, 1)), "B's arrived");
    assert_eq!(recent_of(&a, "web"), Some((1_700_000_800, 1)));
    // The cloud holds A's change and both recents.
    let document = remote_document(&server);
    assert_eq!(document.source_platform(), "linux");
    assert_eq!(remote_recent(&server, "db"), Some((1_700_000_700, 1)));
    assert_eq!(remote_recent(&server, "web"), Some((1_700_000_800, 1)));
    expect_up_to_date(a.service.sync());
    expect_pulled(b.service.sync());
    assert_eq!(address_of(&b, "web"), "10.7.7.7");
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(b.service.sync());
    expect_up_to_date(a.service.sync());
}

#[test]
fn both_sides_only_connecting_keep_the_later_entry_per_host() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-recents", &server);

    // Scenario two: both only connect, partly to the same host.
    a.edit(|state| {
        connect_to(state, "web", 100);
        connect_to(state, "db", 300);
    });
    expect_uploaded(a.service.sync());
    b.edit(|state| {
        connect_to(state, "web", 200);
        connect_to(state, "db", 50);
    });
    expect_merged(b.service.sync());
    assert_eq!(recent_of(&b, "web"), Some((200, 1)), "B's is later");
    assert_eq!(recent_of(&b, "db"), Some((300, 1)), "A's is later");
    assert_eq!(remote_recent(&server, "web"), Some((200, 1)));
    expect_pulled(a.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());

    // When the cloud's entries are the later ones it already holds the result:
    // nothing is uploaded.
    a.edit(|state| connect_to(state, "web", 400));
    expect_uploaded(a.service.sync());
    b.edit(|state| connect_to(state, "web", 350));
    let before = server.requests().len();
    expect_merged(b.service.sync());
    assert_eq!(puts_since(&server, before), 0, "{:?}", server.requests());
    assert_eq!(recent_of(&b, "web"), Some((400, 2)));
    expect_up_to_date(b.service.sync());
    expect_up_to_date(a.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);
}

#[test]
fn both_sides_changing_records_is_still_a_conflict() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-conflict", &server);
    a.edit(|state| {
        readdress(state, "web", "10.1.1.1");
        connect_to(state, "web", 1_000);
    });
    expect_uploaded(a.service.sync());
    b.edit(|state| {
        readdress(state, "db", "10.2.2.2");
        connect_to(state, "db", 2_000);
    });
    let files = data_files(&b.root);
    let remote_before = server.file(REMOTE_PATH).expect("remote");
    expect_conflict(b.service.sync());
    assert_eq!(data_files(&b.root), files, "nothing changed here");
    assert_eq!(server.file(REMOTE_PATH).expect("remote"), remote_before);
    // The same host changed on both sides as well.
    b.edit(|state| readdress(state, "web", "10.3.3.3"));
    expect_conflict(b.service.sync());
    // "Upload now" and "pull" never merge, even when only recents differ here.
    let c = Machine::new("merge-conflict-c", Platform::Linux);
    c.connect(&server);
    expect_pulled(c.service.sync());
    c.edit(|state| connect_to(state, "db", 3_000));
    a.edit(|state| readdress(state, "db", "10.4.4.4"));
    expect_uploaded(a.service.sync());
    expect_conflict(c.service.upload());
    expect_conflict(c.service.pull());
}

#[test]
fn a_state_from_before_the_merge_keeps_the_conflict_rule_once() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-old-state", &server);
    // The state an older build wrote has no fingerprint without recents.
    let store = crate::SyncStateStore::at(&b.root);
    let mut state = store.load();
    assert!(state.last_synced_core_hash.is_some());
    assert!(state.last_remote_core_hash.is_some());
    state.last_synced_core_hash = None;
    state.last_remote_core_hash = None;
    store.save(&state).expect("saved");
    let text = std::fs::read_to_string(store.path()).expect("state");
    assert!(!text.contains("core_hash"), "{text}");

    a.edit(|state| readdress(state, "web", "10.5.5.5"));
    expect_uploaded(a.service.sync());
    b.edit(|state| connect_to(state, "db", 1_000));
    expect_conflict(b.service.sync());
    expect_pulled(b.service.resolve(Resolution::UseRemote));
    assert!(b.service.state().last_synced_core_hash.is_some());
    assert!(b.service.state().last_remote_core_hash.is_some());

    // From now on the merge applies.
    a.edit(|state| readdress(state, "web", "10.6.6.6"));
    expect_uploaded(a.service.sync());
    b.edit(|state| connect_to(state, "db", 2_000));
    expect_merged(b.service.sync());
    assert_eq!(address_of(&b, "web"), "10.6.6.6");
    assert_eq!(recent_of(&b, "db"), Some((2_000, 1)));
}

/// A Windows machine and a Linux machine that synced `web` and `db` (Windows
/// uploaded them, Linux pulled them).
fn synced_cross_platform_pair(name: &str, server: &WebDavServer) -> (Machine, Machine) {
    let windows = Machine::new(&format!("{name}-windows"), Platform::Windows);
    let linux = Machine::new(&format!("{name}-linux"), Platform::Linux);
    windows.connect(server);
    linux.connect(server);
    windows.edit(|state| {
        add_host(state, "web");
        add_host(state, "db");
    });
    expect_uploaded(windows.service.sync());
    expect_pulled(linux.service.sync());
    (windows, linux)
}

#[test]
fn across_platforms_a_cloud_that_only_connected_merges_with_records_changed_here() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_cross_platform_pair("merge-cross-remote", &server);

    // Linux B changes a record; Windows A only connects and uploads. The cloud
    // was last written by Windows, so its Linux view never equalled B's data.
    b.edit(|state| readdress(state, "web", "10.3.0.1"));
    a.edit(|state| connect_to(state, "db", 1_700_001_000));
    expect_uploaded(a.service.sync());
    let report = expect_merged(b.service.sync());
    assert!(report.snapshot.is_some());
    assert_eq!(address_of(&b, "web"), "10.3.0.1", "B's change stays");
    assert_eq!(recent_of(&b, "db"), Some((1_700_001_000, 1)), "A's arrived");
    assert_eq!(remote_recent(&server, "db"), Some((1_700_001_000, 1)));
    expect_up_to_date(b.service.sync());
    expect_pulled(a.service.sync());
    assert_eq!(address_of(&a, "web"), "10.3.0.1");
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());

    // The same after B pulled a record A added on the other platform.
    a.edit(|state| {
        add_host(state, "cache");
    });
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());
    b.edit(|state| readdress(state, "cache", "10.3.0.2"));
    a.edit(|state| connect_to(state, "web", 1_700_001_100));
    expect_uploaded(a.service.sync());
    expect_merged(b.service.sync());
    assert_eq!(address_of(&b, "cache"), "10.3.0.2");
    assert_eq!(recent_of(&b, "web"), Some((1_700_001_100, 1)));
    expect_pulled(a.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);

    // Records changed on both platforms are still a conflict.
    a.edit(|state| readdress(state, "db", "10.3.0.3"));
    expect_uploaded(a.service.sync());
    b.edit(|state| readdress(state, "web", "10.3.0.4"));
    expect_conflict(b.service.sync());
}

#[test]
fn across_platforms_connecting_here_merges_with_records_changed_in_the_cloud() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_cross_platform_pair("merge-cross-local", &server);

    // Windows A only connects; Linux B changes a record and uploads.
    a.edit(|state| connect_to(state, "web", 1_700_002_000));
    b.edit(|state| readdress(state, "db", "10.4.0.1"));
    expect_uploaded(b.service.sync());
    expect_merged(a.service.sync());
    assert_eq!(address_of(&a, "db"), "10.4.0.1", "B's change arrived");
    assert_eq!(recent_of(&a, "web"), Some((1_700_002_000, 1)), "A's kept");
    assert_eq!(remote_recent(&server, "web"), Some((1_700_002_000, 1)));
    expect_up_to_date(a.service.sync());
    expect_pulled(b.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(b.service.sync());

    // The other way round: Linux B only connects and uploads; Windows A changes
    // a record.
    b.edit(|state| connect_to(state, "db", 1_700_002_100));
    expect_uploaded(b.service.sync());
    a.edit(|state| readdress(state, "web", "10.4.0.2"));
    expect_merged(a.service.sync());
    assert_eq!(address_of(&a, "web"), "10.4.0.2");
    assert_eq!(recent_of(&a, "db"), Some((1_700_002_100, 1)));
    expect_pulled(b.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());
}

#[test]
fn the_same_records_on_both_sides_merge_whatever_the_last_sync_recorded() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-same-core", &server);
    // Both sides made the same change to the records (as after a merge whose
    // upload went through but whose local write did not) and connected
    // differently: nothing of either side's records can be lost.
    a.edit(|state| {
        readdress(state, "web", "10.5.0.1");
        connect_to(state, "web", 100);
    });
    expect_uploaded(a.service.sync());
    b.edit(|state| {
        readdress(state, "web", "10.5.0.1");
        connect_to(state, "db", 200);
    });
    expect_merged(b.service.sync());
    assert_eq!(address_of(&b, "web"), "10.5.0.1");
    assert_eq!(recent_of(&b, "web"), Some((100, 1)));
    assert_eq!(recent_of(&b, "db"), Some((200, 1)));
    expect_pulled(a.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);
    expect_up_to_date(a.service.sync());
    expect_up_to_date(b.service.sync());
}

#[test]
fn a_failed_or_refused_merge_upload_leaves_this_machine_as_it_was() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let (a, b) = synced_pair("merge-failure", &server);
    a.edit(|state| readdress(state, "web", "10.9.9.1"));
    expect_uploaded(a.service.sync());
    b.edit(|state| connect_to(state, "db", 1_000));
    let files = data_files(&b.root);

    // The server refuses the write: nothing was written here.
    server.set_read_only(true);
    let error = b.service.sync().expect_err("refused");
    assert!(
        matches!(error, SyncError::Remote(WebDavError::NotWritable { .. })),
        "{error:?}"
    );
    assert_eq!(data_files(&b.root), files);
    server.set_read_only(false);

    // A write refused because the file changed decides again and then merges.
    let before = server.requests().len();
    server.fail_next_put();
    expect_merged(b.service.sync());
    assert_eq!(puts_since(&server, before), 2);
    assert_eq!(address_of(&b, "web"), "10.9.9.1");
    assert_eq!(recent_of(&b, "db"), Some((1_000, 1)));
    assert_eq!(remote_recent(&server, "db"), Some((1_000, 1)));
    expect_up_to_date(b.service.sync());
    expect_pulled(a.service.sync());
    assert_eq!(a.load().hosts, b.load().hosts);
}

#[test]
fn with_passwords_synced_a_password_changed_in_the_cloud_is_not_only_recents() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("merge-secrets-a", Platform::Linux);
    let b = Machine::new("merge-secrets-b", Platform::Linux);
    for machine in [&a, &b] {
        machine.connect(&server);
        machine.update_config(|config| config.sync_secrets = true);
        machine
            .service
            .set_passphrase(Some(&SecretString::new(PASSPHRASE)))
            .expect("saved");
    }
    let mut web_id = None;
    a.edit(|state| {
        let mut web = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("host");
        web.set_auth(Some(AuthMethodKind::Password));
        web.set_remember_secret(true);
        web_id = Some(web.id());
        state.hosts.insert(web).expect("inserted");
        add_host(state, "db");
    });
    let web_id = web_id.expect("web");
    let password_of = |machine: &Machine| {
        machine
            .secrets
            .load(web_id)
            .expect("read")
            .map(|secret| secret.expose().to_owned())
    };
    a.secrets
        .save(web_id, &SecretString::new("first"))
        .expect("saved");
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());
    assert_eq!(password_of(&b).as_deref(), Some("first"));

    // A changes only the password (the records stay as they were). B changes a
    // record: the cloud's change is a password, so this is a conflict, not a
    // merge that would upload B's older password over it.
    a.secrets
        .save(web_id, &SecretString::new("second"))
        .expect("saved");
    expect_uploaded(a.service.sync());
    b.edit(|state| {
        readdress(state, "db", "10.1.2.3");
        connect_to(state, "db", 1_000);
    });
    let remote_before = server.file(REMOTE_PATH).expect("remote");
    expect_conflict(b.service.sync());
    assert_eq!(server.file(REMOTE_PATH).expect("remote"), remote_before);
    assert_eq!(password_of(&b).as_deref(), Some("first"));
    expect_pulled(b.service.resolve(Resolution::UseRemote));
    assert_eq!(password_of(&b).as_deref(), Some("second"));

    // When B only connected, the cloud's password is taken with the merge.
    a.secrets
        .save(web_id, &SecretString::new("third"))
        .expect("saved");
    expect_uploaded(a.service.sync());
    b.edit(|state| connect_to(state, "web", 2_000));
    expect_merged(b.service.sync());
    assert_eq!(password_of(&b).as_deref(), Some("third"));
    assert_eq!(recent_of(&b, "web"), Some((2_000, 1)));
    assert_eq!(remote_recent(&server, "web"), Some((2_000, 1)));
    expect_up_to_date(b.service.sync());
    expect_pulled(a.service.sync());
    assert_eq!(password_of(&a).as_deref(), Some("third"));
    expect_up_to_date(a.service.sync());

    // A passphrase that does not open the cloud's passwords cannot tell what
    // changed there: a conflict, nothing written.
    a.secrets
        .save(web_id, &SecretString::new("fourth"))
        .expect("saved");
    expect_uploaded(a.service.sync());
    b.service
        .set_passphrase(Some(&SecretString::new("another passphrase")))
        .expect("saved");
    b.edit(|state| connect_to(state, "web", 3_000));
    let files = data_files(&b.root);
    expect_conflict(b.service.sync());
    assert_eq!(data_files(&b.root), files);
}

#[test]
fn turning_password_sync_off_here_is_kept_when_the_cloud_only_connected() {
    let server = WebDavServer::start(USER, DAV_PASSWORD);
    let a = Machine::new("merge-secrets-off-a", Platform::Linux);
    let b = Machine::new("merge-secrets-off-b", Platform::Linux);
    for machine in [&a, &b] {
        machine.connect(&server);
        machine.update_config(|config| config.sync_secrets = true);
        machine
            .service
            .set_passphrase(Some(&SecretString::new(PASSPHRASE)))
            .expect("saved");
    }
    let mut web_id = None;
    a.edit(|state| {
        let mut web = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("host");
        web.set_auth(Some(AuthMethodKind::Password));
        web.set_remember_secret(true);
        web_id = Some(web.id());
        state.hosts.insert(web).expect("inserted");
        add_host(state, "db");
    });
    let web_id = web_id.expect("web");
    a.secrets
        .save(web_id, &SecretString::new(HOST_PASSWORD))
        .expect("saved");
    expect_uploaded(a.service.sync());
    expect_pulled(b.service.sync());
    assert!(remote_text(&server).contains("\"secrets\""));

    // A turns password sync off (its records match the cloud's); B only
    // connects and uploads. The merge keeps A's change: the passwords leave
    // the cloud, and the next sync has nothing left to do.
    a.update_config(|config| config.sync_secrets = false);
    b.edit(|state| connect_to(state, "db", 1_000));
    expect_uploaded(b.service.sync());
    assert!(remote_text(&server).contains("\"secrets\""));
    expect_merged(a.service.sync());
    let text = remote_text(&server);
    assert!(!text.contains("\"secrets\""), "{text}");
    assert_eq!(recent_of(&a, "db"), Some((1_000, 1)));
    assert_eq!(remote_recent(&server, "db"), Some((1_000, 1)));
    expect_up_to_date(a.service.sync());
}
