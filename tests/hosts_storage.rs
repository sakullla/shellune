//! The saved host records, as the application really uses them (R3).
//!
//! These tests drive the storage crate through the file system rather than through
//! an in-memory double: a restart is a second store on the same directory, a damaged
//! file is written by the test, and a failed save is forced by making the platform
//! refuse the write. The two properties that matter most - a save that did not
//! finish is never reported as saved, and a secret has nowhere to be written - are
//! asserted against the bytes on disk and the log lines the store emitted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use shellune_core::{
    ConnectionRecord, HostConfig, HostId, HostValidationError, JumpFault, Port, ProtocolKind,
    RdpCertificatePolicy, RdpConfig, SerialConfig, SerialFlow, SerialParity, VncConfig,
    VncSecurity,
};
use shellune_storage::{
    ApplyMode, BookError, CONFIG_KEYS, ExportOptions, FORMAT_VERSION, HostBook, HostEdit,
    HostStore, LOG_FIELDS, LocalState, Platform, PortableError, ShelluneData, StorageError,
    Touched, rebase_state,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A directory that removes itself, so a failing assertion leaves no state behind.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "shellune-storage-{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            tag
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test directory can be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn store(&self) -> HostStore {
        HostStore::at(&self.path)
    }

    fn file(&self) -> PathBuf {
        self.path.join(shellune_storage::FILE_NAME)
    }

    fn write_file(&self, text: &str) {
        std::fs::create_dir_all(&self.path).expect("the test directory exists");
        std::fs::write(self.file(), text).expect("the test can write the host file");
    }

    fn read_file(&self) -> String {
        std::fs::read_to_string(self.file()).expect("the host file exists")
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.path)
            .expect("the test directory is readable")
            .map(|entry| {
                entry
                    .expect("a readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn host(name: &str, address: &str, port: u16, user: &str, group: Option<&str>) -> HostConfig {
    let mut host = HostConfig::new(name, address, port, user).expect("a valid test host");
    host.set_group(group);
    host
}

fn saved(store: &HostStore, host: HostConfig) -> HostConfig {
    let mut book = store.load().expect("the book loads");
    book.apply(&HostEdit::create(host.clone()))
        .expect("a fresh host is inserted");
    store.save(&book).expect("the book saves");
    host
}

// ---------------------------------------------------------------------------
// Create, view, edit, delete, restart
// ---------------------------------------------------------------------------

#[test]
fn a_created_host_survives_a_restart_with_the_fields_it_was_given() {
    let dir = TempDir::new("create");
    let store = dir.store();

    let before = store.load().expect("a first run is not a failure");
    assert_eq!(before.hosts().len(), 0, "a first run starts empty");
    assert_eq!(before.version(), FORMAT_VERSION);

    let host = saved(
        &store,
        host("build", "10.0.0.7", 2222, "deploy", Some("Prod")),
    );

    // The restart: a second store on the same directory, exactly as the next launch
    // would build it.
    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(restarted.hosts().len(), 1);
    let restored = restarted.host(host.id()).expect("the host is still there");
    assert_eq!(restored, &host);
    assert_eq!(restored.target(), "deploy@10.0.0.7:2222");
    assert_eq!(restored.group(), Some("Prod"));
}

#[test]
fn an_edited_host_keeps_its_identity_and_changes_what_the_next_connection_uses() {
    let dir = TempDir::new("edit");
    let store = dir.store();
    let original = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));

    let mut edited = original.clone();
    edited
        .set_address("web.example.org")
        .expect("a valid address");
    edited.set_user("ops").expect("a valid user");
    edited.set_port(2200).expect("a valid port");
    let mut book = store.load().expect("the book loads");
    book.apply(&HostEdit::replace(edited.clone()))
        .expect("the saved host is replaced");
    store.save(&book).expect("the edited book saves");

    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    let restored = restarted.host(original.id()).expect("the same record");
    assert_eq!(
        restored.id(),
        original.id(),
        "an edit must not create a second host"
    );
    assert_eq!(restored.name(), "web", "an untouched field keeps its value");
    assert_eq!(restored.target(), "ops@web.example.org:2200");
    assert_eq!(restarted.hosts().len(), 1);
}

#[test]
fn an_edit_of_a_host_that_is_not_saved_is_refused() {
    let dir = TempDir::new("edit-unknown");
    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let stranger = host("web", "10.0.0.5", 22, "deploy", None);

    let error = book
        .apply(&HostEdit::replace(stranger))
        .expect_err("an unknown identifier cannot be replaced");
    assert!(matches!(error, BookError::UnknownHost { .. }));
    assert!(matches!(book.insert(host("a", "b", 22, "c", None)), Ok(())));
    let saved = book.hosts()[0].clone();
    let error = book
        .insert(saved)
        .expect_err("the same identifier cannot be inserted twice");
    assert!(matches!(error, BookError::DuplicateHost { .. }));
}

#[test]
fn a_deleted_host_and_its_recency_record_are_gone_after_a_restart() {
    let dir = TempDir::new("delete");
    let store = dir.store();
    let keep = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let drop = saved(&store, host("db", "10.0.0.6", 22, "deploy", None));

    let mut book = store.load().expect("the book loads");
    book.record_connection(drop.id(), 1_700_000_000)
        .expect("the connection is recorded");
    book.remove(drop.id()).expect("the host is removed");
    store.save(&book).expect("the removal is saved");

    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(restarted.hosts().len(), 1);
    assert!(restarted.host(keep.id()).is_some());
    assert!(restarted.host(drop.id()).is_none());
    assert!(
        restarted.recent_of(drop.id()).is_none(),
        "the recency of a deleted host must not outlive it"
    );
}

#[test]
fn deleting_the_last_host_leaves_an_empty_book_that_still_loads() {
    let dir = TempDir::new("delete-last");
    let store = dir.store();
    let only = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));

    let mut book = store.load().expect("the book loads");
    book.remove(only.id()).expect("the host is removed");
    store.save(&book).expect("the empty book saves");

    let restarted = HostStore::at(dir.path())
        .load()
        .expect("an empty book still loads");
    assert_eq!(restarted.hosts().len(), 0);
    assert!(
        dir.file().is_file(),
        "the file still exists, it just holds nothing"
    );
}

// ---------------------------------------------------------------------------
// Search, grouping, recents
// ---------------------------------------------------------------------------

#[test]
fn search_grouping_and_recents_all_read_the_same_records() {
    let dir = TempDir::new("views");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", Some("Prod")));
    let _api = saved(
        &store,
        host("api", "api.example.org", 2222, "svc", Some("prod")),
    );
    let db = saved(
        &store,
        host("db", "10.0.0.6", 22, "postgres", Some("Staging")),
    );
    let _lab = saved(&store, host("lab", "lab.internal", 22, "root", None));

    let mut book = store.load().expect("the book loads");
    book.record_connection(db.id(), 1_600_000_000)
        .expect("recorded");
    book.record_connection(web.id(), 1_700_000_000)
        .expect("recorded");
    book.record_connection(web.id(), 1_700_000_100)
        .expect("recorded twice");
    store.save(&book).expect("the book saves");
    let book = HostStore::at(dir.path()).load().expect("the file loads");

    // Search matches name, address, user and group, case-insensitively.
    let names = |hosts: Vec<&HostConfig>| -> Vec<String> {
        hosts.iter().map(|host| host.name().to_owned()).collect()
    };
    assert_eq!(names(book.search("")), vec!["web", "api", "db", "lab"]);
    assert_eq!(
        names(book.search("prod")),
        vec!["web", "api"],
        "the group label is searched"
    );
    assert_eq!(
        names(book.search("10.0.0")),
        vec!["web", "db"],
        "the address is searched"
    );
    assert_eq!(
        names(book.search("POSTGRES")),
        vec!["db"],
        "the user is searched"
    );

    // Grouping consumes the same records, including a filtered list.
    let groups = HostBook::group(book.search(""));
    let labels: Vec<Option<&str>> = groups.iter().map(|group| group.label()).collect();
    assert_eq!(
        labels,
        vec![Some("Prod"), Some("Staging"), None],
        "named groups come alphabetically first, ungrouped hosts last"
    );
    assert_eq!(
        groups[0].len(),
        2,
        "the two case variants of one label are one group"
    );
    assert_eq!(groups[2].hosts()[0].name(), "lab");
    let filtered = HostBook::group(book.search("deploy"));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].len(), 1);

    // The recent view uses the same records and is newest first.
    let recent: Vec<&str> = book
        .recent_hosts()
        .iter()
        .map(|recent| recent.host.name())
        .collect();
    assert_eq!(recent, vec!["web", "db"]);
    assert_eq!(book.recent_hosts()[0].connections, 2);

    // A host with no recorded connection is not in the recent view, and a deleted
    // host disappears from it.
    assert!(!recent.contains(&"api") && !recent.contains(&"lab"));
    let mut book = book;
    book.remove(db.id()).expect("the host is removed");
    let recent: Vec<&str> = book
        .recent_hosts()
        .iter()
        .map(|recent| recent.host.name())
        .collect();
    assert_eq!(recent, vec!["web"]);
}

#[test]
fn a_recency_record_for_an_unknown_host_is_refused() {
    let dir = TempDir::new("recent-unknown");
    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let error = book
        .record_connection(shellune_core::HostId::new(), 1)
        .expect_err("an unknown host cannot be recorded");
    assert!(matches!(error, BookError::UnknownHost { .. }));
    assert!(book.recents().is_empty());
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

#[test]
fn an_invalid_port_is_rejected_by_the_domain_before_anything_is_written() {
    let dir = TempDir::new("port");
    let store = dir.store();
    saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let before = dir.read_file();

    assert_eq!(
        HostConfig::new("web", "10.0.0.9", 0, "deploy"),
        Err(HostValidationError::PortOutOfRange { value: 0 })
    );
    assert_eq!(
        Port::new(65_536),
        Err(HostValidationError::PortOutOfRange { value: 65_536 })
    );
    let mut edited = store.load().expect("the book loads").hosts()[0].clone();
    assert_eq!(
        edited.set_port(0),
        Err(HostValidationError::PortOutOfRange { value: 0 })
    );
    assert_eq!(
        edited.port().get(),
        22,
        "a rejected edit keeps the saved port"
    );

    assert_eq!(
        dir.read_file(),
        before,
        "a rejected input must not reach the file"
    );
}

// ---------------------------------------------------------------------------
// Damage and failure
// ---------------------------------------------------------------------------

#[test]
fn a_damaged_file_is_reported_with_its_path_and_reason() {
    let dir = TempDir::new("damaged");
    dir.write_file("{ this is not a host book }");

    let error = dir
        .store()
        .load()
        .expect_err("a damaged file is not an empty book");
    assert!(
        matches!(error, StorageError::Corrupt { .. }),
        "got {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("hosts.json"),
        "the message names the file: {message}"
    );
    assert!(
        message.len() > "the saved hosts at".len() + 20,
        "the message carries a reason: {message}"
    );
}

#[test]
fn a_file_from_a_newer_build_is_named_instead_of_called_damaged() {
    let dir = TempDir::new("newer");
    dir.write_file(r#"{"version":99,"hosts":[],"recents":[],"something_new":true}"#);

    let error = dir.store().load().expect_err("a newer file is refused");
    assert!(
        matches!(
            error,
            StorageError::UnsupportedVersion {
                found: 99,
                supported: FORMAT_VERSION,
                ..
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn a_file_that_stores_an_invalid_port_is_refused_instead_of_becoming_connectable() {
    let dir = TempDir::new("stored-port");
    let host = host("web", "10.0.0.5", 22, "deploy", None);
    dir.write_file(&format!(
        r#"{{"version":1,"hosts":[{{"id":"{}","name":"web","address":"10.0.0.5","port":0,"user":"deploy","group":null}}],"recents":[]}}"#,
        host.id()
    ));

    let error = dir.store().load().expect_err("port 0 cannot be a host");
    assert!(
        matches!(error, StorageError::Corrupt { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains('0'),
        "the reason names the rejected port: {error}"
    );
}

#[test]
fn a_file_that_carries_a_password_is_refused_rather_than_loaded() {
    let dir = TempDir::new("stored-password");
    let host = host("web", "10.0.0.5", 22, "deploy", None);
    dir.write_file(&format!(
        r#"{{"version":1,"hosts":[{{"id":"{}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null,"password":"hunter2"}}],"recents":[]}}"#,
        host.id()
    ));

    let error = dir
        .store()
        .load()
        .expect_err("a store holding a password is not loaded");
    assert!(
        matches!(error, StorageError::Corrupt { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("unknown field"),
        "the reason names the offending field: {error}"
    );
    assert!(
        !error.to_string().contains("hunter2"),
        "the reason must not repeat the secret: {error}"
    );
}

#[test]
fn a_file_with_two_records_for_one_host_is_refused() {
    let dir = TempDir::new("duplicate");
    let host = host("web", "10.0.0.5", 22, "deploy", None);
    let record = format!(
        r#"{{"id":"{}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}"#,
        host.id()
    );
    dir.write_file(&format!(
        r#"{{"version":1,"hosts":[{record},{record}],"recents":[]}}"#
    ));

    let error = dir
        .store()
        .load()
        .expect_err("two records with one id are not a book");
    assert!(
        matches!(error, StorageError::Corrupt { .. }),
        "got {error:?}"
    );
    assert!(error.to_string().contains("share the id"));
}

#[test]
fn a_data_directory_that_is_a_file_is_reported_as_a_read_failure() {
    let dir = TempDir::new("root-file");
    let root = dir.path().join("not-a-directory");
    std::fs::write(&root, "sentinel").expect("the sentinel file is written");

    let store = HostStore::at(&root);
    let error = store
        .load()
        .expect_err("a directory that is a file cannot hold records");
    assert!(matches!(error, StorageError::Read { .. }), "got {error:?}");
    assert_eq!(
        std::fs::read_to_string(&root).expect("the sentinel is still readable"),
        "sentinel",
        "a failed read must not touch the file it read"
    );
}

#[test]
fn a_save_into_an_impossible_directory_fails_and_keeps_the_previous_state() {
    let dir = TempDir::new("save-impossible");
    let root = dir.path().join("not-a-directory");
    std::fs::write(&root, "sentinel").expect("the sentinel file is written");
    let store = HostStore::at(&root);
    let mut book = HostBook::new();
    book.insert(host("web", "10.0.0.5", 22, "deploy", None))
        .expect("the host is inserted");

    let error = store
        .save(&book)
        .expect_err("a directory that is a file cannot be written");
    assert!(
        matches!(
            error,
            StorageError::CreateDirectory { .. } | StorageError::Write { .. }
        ),
        "got {error:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&root).expect("the sentinel is still readable"),
        "sentinel",
        "a failed save must not touch what is at the target path"
    );
}

#[test]
fn a_successful_save_leaves_no_temporary_file_behind() {
    let dir = TempDir::new("temp-name");
    let store = dir.store();
    saved(&store, host("web", "10.0.0.5", 22, "deploy", None));

    assert_eq!(
        dir.entries(),
        vec![shellune_storage::FILE_NAME.to_owned()],
        "the atomic write must clean up after itself"
    );
}

#[cfg(windows)]
#[test]
// The read-only attribute is what refuses the replace, and the test clears it again
// on a file inside its own temporary directory.
#[allow(clippy::permissions_set_readonly_false)]
fn a_replace_the_platform_refuses_keeps_the_saved_file_intact() {
    let dir = TempDir::new("readonly");
    let store = dir.store();
    let original = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let before = dir.read_file();

    // A read-only host file is what a locked-down profile looks like: the temporary
    // file can be written, but replacing the target is refused.
    let mut permissions = std::fs::metadata(dir.file())
        .expect("the host file exists")
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(dir.file(), permissions).expect("the file can be made read-only");
    let read_only = std::fs::metadata(dir.file())
        .expect("the host file exists")
        .permissions()
        .readonly();

    let mut book = store.load().expect("a read-only file is still readable");
    let mut edited = original;
    edited
        .set_address("web.example.org")
        .expect("a valid address");
    book.apply(&HostEdit::replace(edited))
        .expect("the edit applies");

    let outcome = store.save(&book);
    let mut permissions = std::fs::metadata(dir.file())
        .expect("the host file still exists")
        .permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(dir.file(), permissions).expect("the file can be made writable again");

    assert!(read_only, "the test could not make the file read-only");
    assert!(
        outcome.is_err(),
        "a read-only target must not be reported as saved"
    );
    let reason = outcome.expect_err("the save failed");
    assert!(
        matches!(
            reason,
            StorageError::Replace { .. } | StorageError::Write { .. }
        ),
        "got {reason:?}"
    );
    assert_eq!(
        dir.read_file(),
        before,
        "the previously saved file must be untouched"
    );
    assert_eq!(
        dir.entries(),
        vec![shellune_storage::FILE_NAME.to_owned()],
        "a refused replace must not leave a temporary file"
    );
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[test]
fn the_written_document_has_no_key_outside_the_allowlist() {
    let dir = TempDir::new("keys");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", Some("Prod")));
    let mut book = store.load().expect("the book loads");
    book.record_connection(web.id(), 1_700_000_000)
        .expect("recorded");
    store.save(&book).expect("the book saves");

    let document: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    let mut keys = Vec::new();
    collect_keys(&document, &mut keys);

    for key in &keys {
        assert!(
            CONFIG_KEYS.contains(&key.as_str()),
            "the saved document uses the key {key:?}, which is not on the allowlist"
        );
        let lower = key.to_lowercase();
        for forbidden in [
            "password",
            "passphrase",
            "secret",
            "token",
            "credential",
            "private",
        ] {
            assert!(
                !lower.contains(forbidden),
                "the saved document has a key that names a secret: {key:?}"
            );
        }
    }
    assert!(
        keys.iter().any(|key| key == "recents"),
        "the document really was the one under test: {keys:?}"
    );
}

/// Collects every object key in a JSON value, at every depth.
fn collect_keys(value: &serde_json::Value, keys: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                keys.push(key.clone());
                collect_keys(value, keys);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_keys(item, keys);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

#[test]
fn the_store_logs_what_it_did_without_logging_a_record() {
    let dir = TempDir::new("log");
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::clone(&lines);
    let store = HostStore::at(dir.path()).logging_to(move |line| {
        capture
            .lock()
            .expect("the log is not poisoned")
            .push(line.to_owned());
    });

    store.load().expect("a first run loads");
    let host = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
    let mut book = store.load().expect("the book loads");
    book.apply(&HostEdit::create(host.clone()))
        .expect("inserted");
    store.save(&book).expect("saved");
    book.remove(host.id()).expect("removed");
    store.save(&book).expect("saved");
    dir.write_file("{ broken");
    let _ = store.load().expect_err("the damaged file is reported");

    let lines = lines.lock().expect("the log is not poisoned").clone();
    assert!(
        lines.iter().any(|line| line.starts_with("storage.saved")),
        "a save must be logged: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("storage.load_failed")),
        "a failed read must be logged: {lines:?}"
    );
    for line in &lines {
        let mut fields = words(line);
        assert_eq!(
            fields.first().map(String::as_str),
            Some(line.split(' ').next().unwrap_or_default()),
            "the line starts with its event name: {line}"
        );
        fields.remove(0);
        for field in fields {
            let key = field.split('=').next().unwrap_or_default();
            assert!(
                LOG_FIELDS.contains(&key),
                "the log line carries the field {key:?}, which is not on the allowlist: {line}"
            );
        }
        for forbidden in ["password", "passphrase", "PRIVATE KEY", "hunter2"] {
            assert!(
                !line.contains(forbidden),
                "a log line must not carry a secret: {line}"
            );
        }
        assert!(
            !line.contains("deploy@") && !line.contains("10.0.0.5"),
            "a log line must not repeat the host record: {line}"
        );
    }
}

/// Splits a log line into its fields, keeping a quoted value in one piece.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ' ' if !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

// ---------------------------------------------------------------------------
// Jump chains and ProxyCommand
// ---------------------------------------------------------------------------

/// A script whose execution would create `marker`. Saving must not run it.
fn side_effect_script(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join("proxy-ran.txt");
    let script = if cfg!(windows) {
        let script = dir.join("should-not-run.cmd");
        std::fs::write(
            &script,
            format!("@echo off\r\necho leaked>\"{}\"\r\n", marker.display()),
        )
        .expect("the script can be written");
        script
    } else {
        let script = dir.join("should-not-run.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho leaked > \"{}\"\n", marker.display()),
        )
        .expect("the script can be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script)
                .expect("the script exists")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions)
                .expect("the script can be marked executable");
        }
        script
    };
    (script, marker)
}

#[test]
fn an_old_host_file_without_jump_or_proxy_fields_still_loads() {
    let dir = TempDir::new("legacy-host");
    let host = host("web", "10.0.0.5", 22, "deploy", None);
    dir.write_file(&format!(
        r#"{{"version":1,"hosts":[{{"id":"{}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}],"recents":[]}}"#,
        host.id()
    ));

    let book = dir
        .store()
        .load()
        .expect("a file from before jump chains loads");
    let restored = book.host(host.id()).expect("the host is there");
    assert_eq!(restored.name(), "web");
    assert!(restored.jumps().is_empty());
    assert_eq!(restored.proxy_command(), None);
    assert_eq!(restored.target(), "deploy@10.0.0.5:22");
}

#[test]
fn jump_references_and_a_proxy_command_survive_a_restart_and_are_not_executed() {
    let dir = TempDir::new("jumps");
    let (script, marker) = side_effect_script(dir.path());
    let secret = "hunter2-host-secret";
    let store = dir.store();
    let bastion = saved(&store, host("bastion", "10.0.0.2", 22, "jump", None));
    let mut web = host("web", "10.0.0.5", 2222, "deploy", Some("Prod"));
    web.set_jumps([bastion.id()]);
    web.set_proxy_command(Some(format!("  {}  ", script.display())));
    let web = saved(&store, web);

    assert!(!marker.exists(), "saving a ProxyCommand must not run it");
    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    let restored = restarted.host(web.id()).expect("the host is still there");
    assert_eq!(restored.jumps(), &[bastion.id()]);
    assert_eq!(
        restored.proxy_command(),
        Some(script.display().to_string()).as_deref()
    );
    assert_eq!(restored, &web);
    assert!(!marker.exists(), "loading a ProxyCommand must not run it");

    let bytes = dir.read_file();
    assert!(
        !bytes.contains(secret),
        "a secret that was never stored must not appear"
    );
    assert!(bytes.contains("proxy_command"));
    assert!(bytes.contains("jumps"));
    let document: serde_json::Value = serde_json::from_str(&bytes).expect("json");
    let mut keys = Vec::new();
    collect_keys(&document, &mut keys);
    for key in &keys {
        assert!(CONFIG_KEYS.contains(&key.as_str()), "unexpected key {key}");
        assert!(
            !key.to_lowercase().contains("password") && !key.to_lowercase().contains("secret"),
            "{key}"
        );
    }
}

#[test]
fn a_cycle_and_a_deleted_jump_are_local_and_the_book_still_loads() {
    let dir = TempDir::new("jump-faults");
    let store = dir.store();
    let mut gateway = host("gateway", "10.0.0.2", 22, "jump", None);
    let mut bastion = host("bastion", "10.0.0.3", 22, "jump", None);
    gateway.set_jumps([bastion.id()]);
    bastion.set_jumps([gateway.id()]);
    let gateway = saved(&store, gateway);
    let bastion = saved(&store, bastion);
    let mut web = host("web", "10.0.0.5", 22, "deploy", None);
    let missing = HostId::new();
    web.set_jumps([missing]);
    let web = saved(&store, web);
    let lab = saved(&store, host("lab", "10.0.0.9", 22, "root", None));

    let book = HostStore::at(dir.path())
        .load()
        .expect("faults do not discard the book");
    assert_eq!(book.hosts().len(), 4);
    assert_eq!(
        book.host(web.id()).expect("web").jumps(),
        &[missing],
        "a bad reference is kept so it can be repaired"
    );

    let faults = book.jump_faults();
    assert!(
        faults.iter().any(|fault| matches!(
            fault,
            JumpFault::Cycle { path, .. }
                if path.contains(&gateway.id()) && path.contains(&bastion.id())
        )),
        "the loop names both hops: {faults:?}"
    );
    assert!(
        faults.iter().any(|fault| matches!(
            fault,
            JumpFault::MissingHop {
                host,
                index: 0,
                missing: id,
                ..
            } if *host == web.id() && *id == missing
        )),
        "the missing hop stays attached to web: {faults:?}"
    );
    assert!(
        faults.iter().all(|fault| match fault {
            JumpFault::MissingHop { host, .. } => *host == web.id(),
            JumpFault::Cycle { path, .. } => path.contains(&gateway.id()),
            JumpFault::UnknownHost { .. } => false,
        }),
        "lab is not part of either fault: {faults:?}"
    );
    let blocked = book
        .connection_jump_fault(web.id())
        .expect("web cannot be connected");
    assert!(blocked.message().contains(&missing.to_string()));
    assert!(blocked.message().contains("Remove that hop"));
    assert!(blocked.message().contains("Nothing was connected"));
    assert!(!blocked.message().contains("hunter2"));
    assert!(book.connection_jump_fault(lab.id()).is_none());

    let mut without_bastion = book;
    without_bastion.remove(bastion.id()).expect("removed");
    store.save(&without_bastion).expect("saved");
    let restarted = HostStore::at(dir.path()).load().expect("loads");
    assert!(restarted.host(bastion.id()).is_none());
    assert_eq!(
        restarted.host(gateway.id()).expect("gateway").jumps(),
        &[bastion.id()],
        "deleting a hop target does not rewrite the chain that cited it"
    );
    let after = restarted
        .connection_jump_fault(gateway.id())
        .expect("now missing");
    assert!(matches!(
        after,
        JumpFault::MissingHop { .. } | JumpFault::Cycle { .. }
    ));
    assert!(after.message().contains("Nothing was connected"));
}

// ---------------------------------------------------------------------------
// Hierarchical groups (R18)
// ---------------------------------------------------------------------------

use shellune_core::{
    AuthMethodKind, GroupBookError, GroupPath, MAX_GROUP_SEGMENT_CHARS, SecretString,
};
use shellune_storage::{
    GROUP_FILE_NAME, GroupError, GroupService, KdfParams, MemorySecrets, UnsealSource,
    VaultContent, VaultStore,
};

impl TempDir {
    fn group_file(&self) -> PathBuf {
        self.path.join(GROUP_FILE_NAME)
    }

    fn service(&self) -> GroupService {
        GroupService::at(&self.path)
    }
}

fn group_path(text: &str) -> GroupPath {
    GroupPath::parse(text).expect("a valid test path")
}

#[test]
fn a_legacy_single_group_is_read_as_one_original_segment() {
    // The file an earlier build wrote: one optional label, no hierarchy anywhere.
    let dir = TempDir::new("legacy-group");
    let id = HostId::new();
    dir.write_file(&format!(
        "{{\"version\":1,\"hosts\":[{{\"id\":\"{id}\",\"name\":\"web\",\"address\":\"10.0.0.5\",\"port\":22,\"user\":\"deploy\",\"group\":\"Prod/Web\"}}],\"recents\":[]}}"
    ));

    let store = dir.store();
    let book = store.load().expect("the legacy file loads");
    let host = book.hosts().first().cloned().expect("the host is there");
    assert_eq!(
        host.group(),
        Some("Prod/Web"),
        "the label itself is untouched"
    );
    let path = host.group_path().expect("a label has a path form");
    assert_eq!(
        path.depth(),
        1,
        "the separator was part of the old name, not a level"
    );
    assert_eq!(path.segments(), ["Prod/Web"]);

    store.save(&book).expect("the book saves");
    let text = dir.read_file();
    assert!(
        text.contains("\"group\": \"Prod/Web\""),
        "the label stays in its own field: {text}"
    );
    assert!(
        !text.contains("group_path"),
        "reading a label does not invent a hierarchy: {text}"
    );
}

#[test]
fn a_canonical_path_and_an_empty_group_survive_a_restart() {
    let dir = TempDir::new("group-restart");
    let service = dir.service();
    let host = saved(
        &dir.store(),
        host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
    );
    let (book, groups) = service.load().expect("both documents load");

    let prod_web = group_path("Prod/Web");
    let empty = group_path("Lab/Empty");
    let groups = service
        .create_group(&groups, &prod_web)
        .expect("the group is new");
    let groups = service
        .create_group(&groups, &empty)
        .expect("the empty group is new");
    let update = service
        .move_hosts(&book, &groups, &[host.id()], Some(&prod_web))
        .expect("the host moves");

    assert_eq!(update.hosts_changed, 1);
    assert_eq!(update.hosts.hosts_in_group(&prod_web), vec![host.id()]);

    // The restart: new stores on the same directory, as the next launch builds them.
    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load again");
    let restored = book.host(host.id()).expect("the host is still there");
    assert_eq!(restored.group_path(), Some(prod_web.clone()));
    assert_eq!(restored.address(), "10.0.0.5");
    assert!(groups.contains(&prod_web), "the hierarchy is saved");
    assert!(
        groups.contains(&empty),
        "an empty group is saved even without hosts"
    );
    assert!(
        groups.contains(&group_path("Prod")),
        "its parent is saved beside it"
    );
    assert_eq!(book.hosts_in_group(&empty).len(), 0);
    assert!(dir.group_file().exists());
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn renaming_a_group_moves_the_desktop_connection_with_it() {
    let dir = TempDir::new("group-rename-connection");
    let service = dir.service();
    let web = saved(
        &dir.store(),
        host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
    );

    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let mut office = RdpConfig::new("Office", "10.0.0.9", Some("alice")).expect("valid");
    office.set_group_path(Some(group_path("Prod")));
    let office = ConnectionRecord::Rdp(office);
    book.insert_connection(office.clone())
        .expect("the desktop connection is inserted");
    store.save(&book).expect("the book saves");

    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");
    let renamed = service
        .rename_group(
            &book,
            &groups,
            &group_path("Prod"),
            &group_path("Production"),
        )
        .expect("the rename is valid");
    assert_eq!(
        renamed.hosts_changed, 2,
        "the host and the desktop connection both moved"
    );

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    assert!(groups.contains(&group_path("Production")));
    assert!(!groups.contains(&group_path("Prod")));
    let moved = book
        .connection(office.id())
        .expect("the connection is still saved");
    assert_eq!(
        moved.group_path().map(|path| path.to_string()),
        Some("Production".to_owned()),
        "the connection followed the rename instead of keeping the old path"
    );
    assert_eq!(
        book.connections_under_group(&group_path("Production")),
        vec![office.id()],
        "a filter on the new name finds it"
    );
    assert!(
        book.connections_under_group(&group_path("Prod")).is_empty(),
        "and nothing is left under the old one"
    );
    assert_eq!(
        book.hosts_under_group(&group_path("Production")),
        vec![web.id()]
    );
}

#[test]
fn deleting_a_group_takes_the_serial_connection_out_of_it() {
    let dir = TempDir::new("group-delete-connection");
    let service = dir.service();
    let web = saved(
        &dir.store(),
        host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
    );

    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let mut console = SerialConfig::new("Console", "COM3").expect("valid");
    console.set_group_path(Some(group_path("Prod")));
    let console = ConnectionRecord::Serial(console);
    book.insert_connection(console.clone())
        .expect("the serial connection is inserted");
    store.save(&book).expect("the book saves");

    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");
    let preview = service
        .preview_deletion(&book, &groups, &group_path("Prod"), None)
        .expect("the preview is available");
    assert_eq!(
        preview.hosts_moved, 2,
        "the confirmation counts the host and the serial connection"
    );

    let (update, _) = service
        .delete_group(&book, &groups, &group_path("Prod"), None)
        .expect("the group is deleted");
    assert_eq!(update.hosts_changed, 2);

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    assert!(!groups.contains(&group_path("Prod")));
    let kept = book
        .connection(console.id())
        .expect("the connection is kept, not deleted with the group");
    assert_eq!(
        kept.group_path(),
        None,
        "it left the group instead of keeping a path no tree holds"
    );
    assert!(
        book.connections_under_group(&group_path("Prod")).is_empty(),
        "no connection still points at the deleted group"
    );
    assert_eq!(book.host(web.id()).map(HostConfig::group_path), Some(None));
}
#[test]
fn renaming_a_group_keeps_every_host_and_its_credential_reference() {
    let dir = TempDir::new("group-rename");
    let service = dir.service();

    let api = saved(&dir.store(), host("api", "10.0.0.6", 22, "deploy", None));
    let mut web = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
    web.set_identity_file(Some("keys/web"));
    web.set_auth(Some(AuthMethodKind::Password));
    web.set_remember_secret(true);
    web.set_jumps(vec![api.id()]);
    let web = saved(&dir.store(), web);

    // The remembered password lives in the vault, filed under the host (R1).
    let secrets = MemorySecrets::new();
    let mut vault = VaultContent::new();
    vault.set_host_secret(web.id(), SecretString::new("hunter2"), "test");
    VaultStore::at(dir.path())
        .save_records(
            &vault,
            &[],
            &[UnsealSource::LocalMachine],
            KdfParams::new(8, 1, 1).expect("cheap parameters"),
            &secrets,
        )
        .expect("the vault is written");

    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");
    let moved = service
        .move_hosts(&book, &groups, &[web.id()], Some(&group_path("Prod")))
        .expect("the host moves");

    let renamed = service
        .rename_group(
            &moved.hosts,
            &moved.groups,
            &group_path("Prod"),
            &group_path("Production"),
        )
        .expect("the rename is valid");
    assert_eq!(renamed.hosts_changed, 1);

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    let restored = book.host(web.id()).expect("the host is still there");
    assert_eq!(restored.group_path(), Some(group_path("Production")));
    assert_eq!(restored.address(), "10.0.0.5");
    assert_eq!(restored.user(), "deploy");
    assert_eq!(restored.identity_file(), Some("keys/web"));
    assert_eq!(restored.auth(), Some(AuthMethodKind::Password));
    assert!(
        restored.remembers_secret(),
        "the saved secret still belongs here"
    );
    assert_eq!(restored.jumps(), [api.id()], "the jump chain is unchanged");
    assert!(groups.contains(&group_path("Production")));
    assert!(!groups.contains(&group_path("Prod")));
    let vault = VaultStore::at(dir.path())
        .open(&UnsealSource::LocalMachine, &secrets)
        .expect("the vault opens");
    assert_eq!(
        vault
            .host_secret(web.id())
            .expect("the secret is still filed under the same host")
            .expose(),
        "hunter2"
    );
    assert!(
        secrets
            .accesses()
            .iter()
            .all(|access| !access.is_host_entry()),
        "no host-<uuid> entry was ever asked for"
    );
}

#[test]
fn deleting_a_group_moves_its_hosts_and_never_removes_them() {
    let dir = TempDir::new("group-delete");
    let service = dir.service();
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let api = saved(&dir.store(), host("api", "10.0.0.6", 22, "deploy", None));
    let elsewhere = saved(&dir.store(), host("lab", "10.0.0.9", 22, "deploy", None));

    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod/Web"))
        .expect("the group is new");
    let groups = service
        .create_group(&groups, &group_path("Lab"))
        .expect("the destination exists");
    let moved = service
        .move_hosts(&book, &groups, &[web.id()], Some(&group_path("Prod/Web")))
        .expect("the host moves");

    let preview = service
        .preview_deletion(&moved.hosts, &moved.groups, &group_path("Prod"), None)
        .expect("the preview is available before anything is written");
    assert_eq!(
        preview.hosts_moved, 1,
        "the confirmation counts the hosts inside the group"
    );
    assert_eq!(
        preview.groups_removed,
        vec![group_path("Prod"), group_path("Prod/Web")],
        "the confirmation names every group that goes"
    );

    let (update, deletion) = service
        .delete_group(&moved.hosts, &moved.groups, &group_path("Prod"), None)
        .expect("the group is deleted");
    assert_eq!(deletion.hosts_moved, 1);
    assert_eq!(update.hosts_changed, 1);

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    assert_eq!(
        book.hosts().len(),
        3,
        "deleting a group never deletes a host"
    );
    assert_eq!(
        book.host(web.id())
            .expect("the host is still saved")
            .group_path(),
        None,
        "its hosts went to where the user chose"
    );
    assert_eq!(
        book.host(api.id())
            .expect("the untouched host is still saved")
            .address(),
        "10.0.0.6"
    );
    assert_eq!(
        book.host(elsewhere.id())
            .expect("another group is untouched")
            .group_path(),
        None
    );
    assert!(!groups.contains(&group_path("Prod")));

    // A second deletion, this time of a group with nothing in it.
    let (update, _) = service
        .delete_group(&book, &groups, &group_path("Lab"), None)
        .expect("the empty group is deleted");
    assert_eq!(update.hosts_changed, 0, "an empty group moves nothing");
    let (book, groups) = service.load().expect("both documents load");
    assert_eq!(book.hosts().len(), 3);
    assert!(!groups.contains(&group_path("Lab")));
}

#[test]
fn a_repeated_group_name_or_an_unknown_host_leaves_both_files_as_they_were() {
    let dir = TempDir::new("group-refused");
    let service = dir.service();
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");

    let hosts_before = dir.read_file();
    let groups_before = std::fs::read_to_string(dir.group_file()).expect("the group file is there");

    assert_eq!(
        service.create_group(&groups, &group_path("Prod")),
        Err(GroupError::Groups(GroupBookError::AlreadyExists {
            path: group_path("Prod"),
        }))
    );
    assert!(matches!(
        service.move_hosts(&book, &groups, &[HostId::new()], Some(&group_path("Prod"))),
        Err(GroupError::Hosts(BookError::UnknownHost { .. }))
    ));
    assert_eq!(
        dir.read_file(),
        hosts_before,
        "a refused move does not rewrite the hosts"
    );
    assert_eq!(
        std::fs::read_to_string(dir.group_file()).expect("the group file is there"),
        groups_before,
        "a refused change does not rewrite the tree"
    );
    assert_eq!(
        book.host(web.id()).expect("the host is saved").group_path(),
        None
    );
}

#[test]
fn a_failed_group_write_puts_the_hosts_back() {
    let dir = TempDir::new("group-half-written");
    let service = dir.service();
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let (book, groups) = service.load().expect("both documents load");
    let hosts_before = dir.read_file();

    // A directory where the group file belongs: the hosts can be written, the tree
    // cannot be replaced.
    std::fs::create_dir_all(dir.group_file()).expect("the test can block the group file");

    match service.move_hosts(&book, &groups, &[web.id()], Some(&group_path("Prod"))) {
        Err(GroupError::HalfWritten { restored, .. }) => {
            assert_eq!(restored, "restored", "the hosts were put back");
        }
        other => panic!("the change must report the failed tree write, got {other:?}"),
    }

    assert_eq!(
        dir.read_file(),
        hosts_before,
        "the hosts are what they were"
    );
    let reloaded = dir.store().load().expect("the host file still loads");
    assert_eq!(
        reloaded
            .host(web.id())
            .expect("the host is still there")
            .group_path(),
        None
    );
}

#[test]
fn moving_a_host_with_a_legacy_label_out_of_its_group_clears_the_label() {
    // The label was written by an earlier build; leaving the group has to clear it,
    // otherwise the host stays filed under the group it was moved out of.
    let dir = TempDir::new("group-move-out");
    let service = dir.service();
    let web = saved(
        &dir.store(),
        host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
    );
    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");
    assert_eq!(
        book.hosts_in_group(&group_path("Prod")),
        vec![web.id()],
        "the label read as the group it meant"
    );

    let update = service
        .move_hosts(&book, &groups, &[web.id()], None)
        .expect("the host leaves every group");
    assert_eq!(update.hosts_changed, 1, "leaving a group is a change");
    let moved = update.hosts.host(web.id()).expect("the host is saved");
    assert_eq!(
        moved.group(),
        None,
        "the old label is cleared, not just the path"
    );
    assert_eq!(moved.group_path(), None);
    let text = dir.read_file();
    assert!(
        !text.contains("\"Prod\""),
        "the saved record carries no label: {text}"
    );

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    let restored = book.host(web.id()).expect("the host is still there");
    assert_eq!(
        restored.group(),
        None,
        "the restart does not bring the label back"
    );
    assert_eq!(restored.group_path(), None);
    assert!(
        book.hosts_in_group(&group_path("Prod")).is_empty(),
        "the group is empty now"
    );
    assert!(
        groups.contains(&group_path("Prod")),
        "the group itself was not deleted"
    );
}

#[test]
fn saving_an_edit_of_a_nested_host_through_the_form_keeps_its_group() {
    // The host form has one group box and saves it with every edit. For a host in a
    // hierarchy that box is empty, so editing the name must not move the host out of
    // the group it is in.
    let dir = TempDir::new("group-form-empty");
    let service = dir.service();
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let (book, groups) = service.load().expect("both documents load");
    let nested = group_path("Prod/Web");
    let groups = service
        .create_group(&groups, &nested)
        .expect("the group is new");
    let update = service
        .move_hosts(&book, &groups, &[web.id()], Some(&nested))
        .expect("the host moves in");
    let mut edited = update
        .hosts
        .host(web.id())
        .expect("the host is saved")
        .clone();
    assert_eq!(edited.group_path(), Some(nested.clone()));

    // Exactly what the form does for an existing host: the name changes and the group
    // box, which cannot show a hierarchy, submits an empty string.
    edited.set_name("web-2").expect("a valid name");
    edited.set_group(Some(String::new()));
    assert_eq!(
        edited.group_path(),
        Some(nested.clone()),
        "an empty group box is not a request to leave the group"
    );
    assert_eq!(
        edited.group(),
        None,
        "the hierarchy carries no legacy label to fall back on"
    );

    let mut book = update.hosts;
    book.apply(&HostEdit::replace(edited))
        .expect("the edited host is stored");
    dir.store().save(&book).expect("the book saves");

    let reloaded = HostStore::at(dir.path()).load().expect("the file loads");
    let restored = reloaded.host(web.id()).expect("the host is still there");
    assert_eq!(restored.name(), "web-2");
    assert_eq!(
        restored.group_path(),
        Some(nested.clone()),
        "the restart still files the host under its group"
    );
    assert_eq!(
        reloaded.hosts_in_group(&nested),
        vec![web.id()],
        "the group still lists the host"
    );
}
#[test]
fn deleting_a_group_also_moves_out_the_hosts_that_named_it_by_label() {
    let dir = TempDir::new("group-delete-legacy");
    let service = dir.service();
    let web = saved(
        &dir.store(),
        host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
    );
    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");

    let (update, deletion) = service
        .delete_group(&book, &groups, &group_path("Prod"), None)
        .expect("the group is deleted");
    assert_eq!(
        deletion.hosts_moved, 1,
        "the confirmation counts the labeled host"
    );
    assert_eq!(update.hosts_changed, 1);

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    assert_eq!(
        book.hosts().len(),
        1,
        "deleting a group never deletes a host"
    );
    let restored = book.host(web.id()).expect("the host is still there");
    assert_eq!(
        restored.group(),
        None,
        "the old label cannot keep the host in a deleted group"
    );
    assert_eq!(restored.group_path(), None);
    assert!(!groups.contains(&group_path("Prod")));
}

#[test]
fn renaming_a_group_under_a_new_parent_can_be_read_back_after_a_restart() {
    let dir = TempDir::new("group-rename-parent");
    let service = dir.service();
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod/Web"))
        .expect("the group is new");
    let moved = service
        .move_hosts(&book, &groups, &[web.id()], Some(&group_path("Prod/Web")))
        .expect("the host moves");

    // "Staging" was never created, so the destination names a parent that is missing.
    let renamed = service
        .rename_group(
            &moved.hosts,
            &moved.groups,
            &group_path("Prod"),
            &group_path("Staging/New"),
        )
        .expect("the rename is valid");
    assert_eq!(renamed.hosts_changed, 1);

    let (book, groups) = GroupService::at(dir.path())
        .load()
        .expect("the saved tree loads again");
    assert_eq!(
        groups.validate(),
        Ok(()),
        "the saved tree is one this build can read back"
    );
    assert!(
        groups.contains(&group_path("Staging")),
        "the parent of the new path is stored with it"
    );
    assert!(groups.contains(&group_path("Staging/New")));
    assert!(groups.contains(&group_path("Staging/New/Web")));
    assert!(
        !groups.contains(&group_path("Prod")),
        "the old path is gone"
    );
    assert_eq!(
        book.host(web.id())
            .expect("the host is still there")
            .group_path(),
        Some(group_path("Staging/New/Web"))
    );
}

#[test]
fn moving_a_host_to_the_group_it_is_already_in_changes_nothing() {
    let dir = TempDir::new("group-no-change");
    let service = dir.service();
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let api = saved(&dir.store(), host("api", "10.0.0.6", 22, "deploy", None));
    let (book, groups) = service.load().expect("both documents load");
    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");
    let first = service
        .move_hosts(&book, &groups, &[web.id()], Some(&group_path("Prod")))
        .expect("the host moves");
    assert_eq!(first.hosts_changed, 1);

    let again = service
        .move_hosts(
            &first.hosts,
            &first.groups,
            &[web.id()],
            Some(&group_path("Prod")),
        )
        .expect("the same move is accepted");
    assert_eq!(
        again.hosts_changed, 0,
        "a host already in the group did not move"
    );

    // One of the two really moves, so the count is one and not two.
    let mixed = service
        .move_hosts(
            &again.hosts,
            &again.groups,
            &[web.id(), api.id()],
            Some(&group_path("Prod")),
        )
        .expect("the selection moves");
    assert_eq!(
        mixed.hosts_changed, 1,
        "only the host that changed group is counted"
    );
}

#[test]
fn a_label_that_cannot_be_a_group_path_is_reported_and_kept_visible() {
    let dir = TempDir::new("group-unusable-label");
    let service = dir.service();
    let long = "n".repeat(MAX_GROUP_SEGMENT_CHARS + 1);
    let odd = saved(
        &dir.store(),
        host("odd", "10.0.0.7", 22, "deploy", Some(&long)),
    );
    let web = saved(&dir.store(), host("web", "10.0.0.5", 22, "deploy", None));
    let (book, groups) = service.load().expect("both documents load");

    let listed = book.legacy_group_labels();
    assert_eq!(listed.len(), 1, "the host is listed instead of dropped");
    assert_eq!(listed[0].host(), odd.id());
    assert_eq!(listed[0].host_name(), "odd");
    assert_eq!(listed[0].label(), long);
    assert_eq!(
        book.host(odd.id()).expect("the host is saved").group_text(),
        Some(long.clone()),
        "the label stays visible beside the host"
    );
    assert_eq!(
        book.host(odd.id()).expect("the host is saved").group_path(),
        None,
        "but it has no place in the tree"
    );

    let groups = service
        .create_group(&groups, &group_path("Prod"))
        .expect("the group is new");
    let preview = service
        .preview_deletion(&book, &groups, &group_path("Prod"), None)
        .expect("the preview is available");
    assert_eq!(preview.hosts_moved, 0);
    assert_eq!(
        preview.labels_without_group.len(),
        1,
        "the confirmation names the host it cannot count in a group"
    );

    let update = service
        .move_hosts(&book, &groups, &[web.id()], Some(&group_path("Prod")))
        .expect("the host moves");
    assert_eq!(update.hosts_changed, 1);
    assert_eq!(
        update.labels_without_group.len(),
        1,
        "the result still names the host it could not place"
    );

    let (book, _) = GroupService::at(dir.path())
        .load()
        .expect("both documents load after the restart");
    assert_eq!(
        book.host(odd.id())
            .expect("the host is still there")
            .group(),
        Some(long.as_str()),
        "the label was not silently dropped"
    );
}
// ---------------------------------------------------------------------------
// Connections of the other protocols (R2, R9, R18)
// ---------------------------------------------------------------------------

/// An RDP connection a test can save.
fn rdp(name: &str, address: &str, user: &str) -> ConnectionRecord {
    let mut config = RdpConfig::new(name, address, Some(user)).expect("a valid RDP connection");
    config.set_port(3_389).expect("the default port");
    ConnectionRecord::Rdp(config)
}

/// A VNC connection a test can save.
fn vnc(name: &str, address: &str) -> ConnectionRecord {
    ConnectionRecord::Vnc(VncConfig::new(name, address).expect("a valid VNC connection"))
}

/// A serial connection a test can save, on a frame that is not the default.
fn serial(name: &str, device: &str) -> ConnectionRecord {
    let mut config = SerialConfig::new(name, device).expect("a valid serial connection");
    config.set_baud(9_600).expect("a common speed");
    config.set_parity(SerialParity::Even);
    config.set_flow(SerialFlow::Hardware);
    ConnectionRecord::Serial(config)
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_document_with_desktop_and_serial_connections_passes_the_persisted_key_audit() {
    let dir = TempDir::new("connection-audit");
    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let mut office = RdpConfig::new("Office", "10.0.0.9", Some("alice")).expect("valid");
    office.set_domain(Some("CORP"));
    office.set_certificate(RdpCertificatePolicy::Reject);
    // The one key whose name says "secret": it is a flag, and the audit has to accept it
    // without accepting any value beside it.
    office.set_remember_secret(true);
    office.set_clipboard(false);
    office
        .set_drive_folder(Some("C:/Users/alice/Exchange"))
        .expect("a usable folder");
    book.insert_connection(ConnectionRecord::Rdp(office))
        .expect("inserted");
    let mut wall = VncConfig::new("Wall", "10.0.0.7").expect("valid");
    wall.set_security(VncSecurity::EncryptedOnly);
    wall.set_clipboard(false);
    book.insert_connection(ConnectionRecord::Vnc(wall))
        .expect("inserted");
    let mut console = SerialConfig::new("Console", "COM3").expect("valid");
    console.set_parity(SerialParity::Even);
    console.set_flow(SerialFlow::Hardware);
    console.set_local_echo(true);
    book.insert_connection(ConnectionRecord::Serial(console))
        .expect("inserted");
    store.save(&book).expect("the book saves");

    let document: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    let mut keys = Vec::new();
    collect_keys(&document, &mut keys);
    for key in &keys {
        assert!(
            CONFIG_KEYS.contains(&key.as_str()),
            "the saved document uses the key {key:?}, which is not on the allowlist"
        );
        let lower = key.to_lowercase();
        if key == "remember_secret" {
            // A boolean flag: it says a credential may be in the system store, and no
            // value of any credential is written beside the record.
            continue;
        }
        for forbidden in [
            "password",
            "passphrase",
            "secret",
            "token",
            "credential",
            "private",
        ] {
            assert!(
                !lower.contains(forbidden),
                "the saved document has a key that names a secret: {key:?}"
            );
        }
    }
    assert_eq!(
        document["connections"][0]["remember_secret"],
        serde_json::Value::Bool(true),
        "the flag says a credential may be in the system store, and holds no value"
    );
    // The audit really covered the new records rather than only an SSH one.
    for expected in [
        "protocol",
        "domain",
        "certificate",
        "security",
        "device",
        "baud",
        "data_bits",
        "parity",
        "stop_bits",
        "flow",
        "local_echo",
        "clipboard",
        "drive_folder",
    ] {
        assert!(
            keys.iter().any(|key| key == expected),
            "the audit did not see the {expected} key: {keys:?}"
        );
    }
}

#[test]
fn the_save_log_counts_connections_apart_from_hosts() {
    let dir = TempDir::new("connection-log");
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let store = dir.store().logging_to(move |line| {
        sink.lock()
            .expect("the log is reachable")
            .push(line.to_owned());
    });
    let mut book = store.load().expect("the book loads");
    book.insert(host("web", "10.0.0.5", 22, "deploy", None))
        .expect("the host is inserted");
    book.insert_connection(serial("Console", "COM3"))
        .expect("the connection is inserted");
    store.save(&book).expect("the book saves");

    let saved = lines
        .lock()
        .expect("the log is reachable")
        .iter()
        .find(|line| line.starts_with("storage.saved"))
        .cloned()
        .expect("the save was logged");
    assert!(
        saved.contains("hosts=1"),
        "one SSH host was written: {saved}"
    );
    assert!(
        saved.contains("connections=1"),
        "one connection of another protocol was written: {saved}"
    );
}
#[test]
fn a_book_of_ssh_hosts_is_written_without_the_new_fields() {
    let dir = TempDir::new("legacy-shape");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", Some("Prod")));

    let book = store.load().expect("the book loads");
    assert_eq!(book.connections().len(), 0);
    assert_eq!(book.connection_count(), 1);
    let record = book.connection(web.id()).expect("the host is a connection");
    assert_eq!(record.protocol(), ProtocolKind::Ssh);
    assert_eq!(
        book.records().len(),
        1,
        "a reader sees the host list and the connection list as one list"
    );

    let document: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    assert!(
        document.get("connections").is_none(),
        "a book without other protocols writes no connections key: {document}"
    );
    let stored_host = &document["hosts"][0];
    assert!(
        stored_host.get("protocol").is_none(),
        "an SSH host keeps the exact shape older builds wrote: {stored_host}"
    );
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_desktop_and_a_serial_connection_survive_a_restart_with_their_protocol() {
    let dir = TempDir::new("protocols");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));

    let mut book = store.load().expect("the book loads");
    let office = rdp("Office desktop", "10.0.0.9", "alice");
    let wall = vnc("Wall", "10.0.0.7");
    let console = serial("Console", "COM3");
    for record in [&office, &wall, &console] {
        book.insert_connection(record.clone())
            .expect("a fresh connection is inserted");
    }
    store.save(&book).expect("the book saves");

    let document: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    let stored = document["connections"]
        .as_array()
        .expect("the other protocols are stored as a list");
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[0]["protocol"], "rdp");
    assert_eq!(stored[1]["protocol"], "vnc");
    assert_eq!(stored[2]["protocol"], "serial");
    assert_eq!(
        document["hosts"].as_array().map(Vec::len),
        Some(1),
        "the SSH host stays in the host list it was written to"
    );

    let book = dir.store().load().expect("the restarted store reads it");
    assert_eq!(book.hosts().len(), 1);
    assert_eq!(book.connections().len(), 3);
    assert_eq!(book.connection_count(), 4);
    assert_eq!(book.connection(office.id()), Some(office.clone()));
    assert_eq!(book.connection(wall.id()), Some(wall.clone()));
    let reread = book
        .connection(console.id())
        .expect("the serial record is there");
    assert_eq!(reread, console.clone());
    let ConnectionRecord::Serial(config) = reread else {
        panic!("the serial record keeps its protocol");
    };
    assert_eq!(config.baud(), 9_600);
    assert_eq!(config.parity(), SerialParity::Even);
    assert_eq!(config.flow(), SerialFlow::Hardware);
    assert_eq!(config.frame(), "8E1");
    assert_eq!(book.host(web.id()).map(HostConfig::name), Some("web"));
}

#[test]
fn an_unknown_protocol_is_reported_instead_of_becoming_an_empty_book() {
    let dir = TempDir::new("unknown-protocol");
    let store = dir.store();
    let id = HostId::new();
    let wall = VncConfig::new("wall", "10.0.0.7").expect("valid");
    let wall_id = wall.id();
    let text = format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[],"connections":[{{"protocol":"telnet","id":"{id}","name":"old switch","address":"10.0.0.2","port":23}},{{"protocol":"vnc","id":"{wall_id}","name":"wall","address":"10.0.0.7","port":5900}}],"recents":[],"favorites":["{id}"]}}"#
    );
    dir.write_file(&text);

    let book = store
        .load()
        .expect("one record this build cannot run does not take the others with it");
    assert_eq!(
        book.records()
            .iter()
            .map(ConnectionRecord::id)
            .collect::<Vec<_>>(),
        vec![wall_id],
        "only the record this build understands can be listed and dialled"
    );
    let [unreadable] = book.unreadable_connections() else {
        panic!(
            "the record is reported: {:?}",
            book.unreadable_connections()
        );
    };
    assert_eq!(unreadable.id(), Some(id));
    assert_eq!(unreadable.name(), Some("old switch"));
    assert_eq!(unreadable.protocol(), Some("telnet"));
    assert!(
        unreadable.reason().contains("telnet"),
        "the report names the protocol it found: {}",
        unreadable.reason()
    );
    assert_eq!(dir.read_file(), text, "reading the file does not change it");

    // Saving keeps the record exactly as it was written, so the build that wrote it
    // still finds it; its favourite mark stays with it.
    store.save(&book).expect("the book saves");
    let written: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    let original: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert!(
        written["connections"]
            .as_array()
            .expect("a list")
            .contains(&original["connections"][0]),
        "the record is written back unchanged: {written}"
    );
    assert_eq!(written["favorites"], original["favorites"]);
    let reread = store.load().expect("the saved file loads again");
    assert_eq!(reread, book);

    // A new record cannot take the identifier the set-aside record uses.
    let mut book = reread;
    let clash = serial("console", "COM3").reidentified(id);
    assert_eq!(
        book.insert_connection(clash),
        Err(BookError::DuplicateHost { id })
    );
}

#[test]
fn a_record_from_a_newer_build_with_an_unknown_field_is_set_aside_on_its_own() {
    // A later build adds a field to an RDP record. This build reports that one
    // record and keeps every other one usable, instead of refusing the whole file.
    let dir = TempDir::new("newer-field");
    let store = dir.store();
    let web = host("web", "10.0.0.5", 22, "deploy", None);
    let web_id = web.id();
    let id = HostId::new();
    let text = format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[{{"id":"{web_id}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}],"connections":[{{"protocol":"rdp","id":"{id}","name":"office","address":"10.0.0.9","port":3389,"clipboard":false,"a_later_option":3}}],"recents":[{{"host":"{id}","last_connected_unix":1700000000,"connections":2}}]}}"#
    );
    dir.write_file(&text);

    let book = store.load().expect("the file loads");
    assert_eq!(book.host(web_id).map(HostConfig::name), Some("web"));
    assert!(book.connections().is_empty());
    let [unreadable] = book.unreadable_connections() else {
        panic!("one record is reported");
    };
    assert_eq!(unreadable.id(), Some(id));
    assert_eq!(unreadable.protocol(), Some("rdp"));
    assert!(
        unreadable.reason().contains("a_later_option"),
        "the report names the field it did not know: {}",
        unreadable.reason()
    );
    assert_eq!(book.recents().len(), 1, "its history stays with it");

    store.save(&book).expect("saves");
    let reread = store.load().expect("loads again");
    assert_eq!(
        reread.unreadable_connections(),
        book.unreadable_connections()
    );
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_desktop_record_inside_the_host_list_is_refused_instead_of_becoming_a_host() {
    let dir = TempDir::new("rdp-in-hosts");
    let store = dir.store();
    let id = HostId::new();
    dir.write_file(&format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[{{"protocol":"rdp","id":"{id}","name":"office","address":"10.0.0.9","port":3389}}],"recents":[]}}"#
    ));
    let error = store
        .load()
        .expect_err("a desktop record is not an SSH host record");
    assert!(
        matches!(error, StorageError::Corrupt { .. }),
        "the file is damaged as far as this build is concerned: {error:?}"
    );
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_record_with_a_field_its_protocol_does_not_have_is_refused() {
    let dir = TempDir::new("foreign-field");
    let store = dir.store();
    let id = HostId::new();
    dir.write_file(&format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[],"connections":[{{"protocol":"vnc","id":"{id}","name":"wall","address":"10.0.0.7","port":5900,"domain":"CORP"}}],"recents":[]}}"#
    ));
    let book = store
        .load()
        .expect("one record this build cannot read does not fail the file");
    assert!(
        book.connections().is_empty(),
        "the VNC record with an RDP domain must stay unreadable"
    );
    let [unreadable] = book.unreadable_connections() else {
        panic!("the record is reported");
    };
    assert_eq!(unreadable.id(), Some(id));
    assert!(
        unreadable.reason().contains("domain"),
        "{}",
        unreadable.reason()
    );
}

#[test]
fn an_ssh_record_in_the_connection_list_is_refused_rather_than_read_twice() {
    let dir = TempDir::new("ssh-in-connections");
    let store = dir.store();
    let host = host("web", "10.0.0.5", 22, "deploy", None);
    let id = host.id();
    dir.write_file(&format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[],"connections":[{{"protocol":"ssh","id":"{id}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}],"recents":[]}}"#
    ));
    let error = store.load().expect_err("one record cannot be two things");
    assert!(
        error.to_string().contains(&id.to_string()),
        "the refusal names the record: {error}"
    );
}

#[test]
fn two_records_with_one_identifier_are_refused_across_the_lists() {
    let dir = TempDir::new("shared-id");
    let store = dir.store();
    let id = HostId::new();
    dir.write_file(&format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[{{"id":"{id}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}],"connections":[{{"protocol":"vnc","id":"{id}","name":"wall","address":"10.0.0.7","port":5900}}],"recents":[]}}"#
    ));
    let error = store
        .load()
        .expect_err("one identifier cannot name two records");
    assert!(
        error.to_string().contains(&id.to_string()),
        "the refusal names the identifier: {error}"
    );
}

#[test]
fn a_connection_identifier_is_unique_across_every_protocol() {
    let dir = TempDir::new("unique-id");
    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let office = rdp("Office", "10.0.0.9", "alice");
    book.insert_connection(office.clone()).expect("inserted");
    assert_eq!(
        book.insert_connection(office.clone()),
        Err(BookError::DuplicateHost { id: office.id() })
    );
    let web = host("web", "10.0.0.5", 22, "deploy", None);
    book.insert(web.clone()).expect("the SSH host is inserted");
    assert_eq!(
        book.insert_connection(ConnectionRecord::Ssh(web.clone())),
        Err(BookError::DuplicateHost { id: web.id() }),
        "the host list and the connection list share one namespace"
    );
    book.record_connection(office.id(), 1_700_000_000)
        .expect("a saved desktop connection records its recency too");
    assert_eq!(
        book.recent_of(office.id())
            .map(|recent| (recent.last_connected_unix(), recent.connections())),
        Some((1_700_000_000, 1))
    );
    let unknown = HostId::new();
    assert_eq!(
        book.record_connection(unknown, 1_700_000_000),
        Err(BookError::UnknownHost { id: unknown }),
        "an unknown identifier is still refused"
    );
}

#[test]
fn changing_a_connection_to_another_protocol_moves_it_and_drops_the_sign_in_history() {
    let dir = TempDir::new("protocol-change");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let mut book = store.load().expect("the book loads");
    book.record_connection(web.id(), 1_700_000_000)
        .expect("the sign-in is recorded");

    let mut office = RdpConfig::new("web", "10.0.0.5", Some("deploy")).expect("valid");
    office.set_port(3_389).expect("a port");
    book.update_connection(
        ConnectionRecord::Rdp(office).reidentified(web.id()),
        web.id(),
    )
    .expect("the connection changes protocol");
    assert!(
        book.update_connection(vnc("wall", "10.0.0.7"), web.id())
            .is_err(),
        "a record under another identifier cannot replace this one"
    );
    assert_eq!(book.hosts().len(), 0, "the host record is gone");
    assert_eq!(book.connections().len(), 1);
    assert_eq!(
        book.recents().len(),
        0,
        "a desktop session does not inherit the SSH sign-in history"
    );
    store.save(&book).expect("the book saves");

    let mut book = dir.store().load().expect("the restarted store reads it");
    assert_eq!(
        book.connection(web.id()).map(|record| record.protocol()),
        Some(ProtocolKind::Rdp)
    );

    let unknown = HostId::new();
    assert_eq!(
        book.update_connection(vnc("wall", "10.0.0.7"), unknown),
        Err(BookError::UnknownHost { id: unknown })
    );
    assert_eq!(
        book.remove_connection(unknown),
        Err(BookError::UnknownHost { id: unknown })
    );
}

#[test]
fn removing_a_connection_takes_it_out_of_the_list_it_lives_in() {
    let dir = TempDir::new("remove-connection");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let mut book = store.load().expect("the book loads");
    book.record_connection(web.id(), 1_700_000_000)
        .expect("the sign-in is recorded");
    let wall = vnc("Wall", "10.0.0.7");
    book.insert_connection(wall.clone()).expect("inserted");

    assert_eq!(
        book.remove_connection(wall.id()),
        Ok(wall.clone()),
        "a desktop connection is removed from its own list"
    );
    assert_eq!(book.connections().len(), 0);
    let saved_web = book.host(web.id()).expect("the host is there").clone();
    assert_eq!(
        book.remove_connection(web.id()),
        Ok(ConnectionRecord::Ssh(saved_web)),
        "the SSH host is still removable the way it always was"
    );
    assert_eq!(book.hosts().len(), 0);
    assert_eq!(
        book.recents().len(),
        0,
        "removing a host also removes its sign-in record"
    );
}

#[test]
fn a_search_finds_a_connection_by_its_name_its_protocol_or_its_target() {
    let dir = TempDir::new("search-connections");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", Some("Prod")));
    let mut book = store.load().expect("the book loads");
    let office = rdp("Office desktop", "10.0.0.9", "alice");
    let console = serial("Console", "COM3");
    book.insert_connection(office.clone()).expect("inserted");
    book.insert_connection(console.clone()).expect("inserted");

    let ids = |phrase: &str| -> Vec<HostId> {
        book.search_connections(phrase)
            .iter()
            .map(ConnectionRecord::id)
            .collect()
    };
    assert_eq!(ids(""), vec![web.id(), office.id(), console.id()]);
    assert_eq!(ids("office"), vec![office.id()]);
    assert_eq!(ids("rdp"), vec![office.id()]);
    assert_eq!(ids("serial"), vec![console.id()]);
    assert_eq!(ids("COM3"), vec![console.id()]);
    assert_eq!(ids("alice@10.0.0.9"), vec![office.id()]);
    assert_eq!(
        ids("prod"),
        vec![web.id()],
        "an SSH connection is still found by its group label"
    );
    assert_eq!(ids("ssh"), vec![web.id()]);
    assert!(ids("warehouse").is_empty());
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_document_with_desktop_connections_still_has_no_place_for_a_secret() {
    let dir = TempDir::new("connection-keys");
    let store = dir.store();
    let mut book = store.load().expect("the book loads");
    let mut office = RdpConfig::new("Office", "10.0.0.9", Some("alice")).expect("valid");
    office.set_remember_secret(true);
    book.insert_connection(ConnectionRecord::Rdp(office))
        .expect("inserted");
    book.insert_connection(serial("Console", "COM3"))
        .expect("inserted");
    store.save(&book).expect("the book saves");

    let document: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    let mut keys = Vec::new();
    collect_keys(&document, &mut keys);
    assert!(
        keys.iter().any(|key| key == "remember_secret"),
        "the document really was the one under test: {keys:?}"
    );
    for key in &keys {
        let lower = key.to_lowercase();
        for forbidden in [
            "password",
            "passphrase",
            "secret_value",
            "token",
            "credential",
            "private",
        ] {
            assert!(
                !lower.contains(forbidden),
                "the saved document has a key that names a secret: {key:?}"
            );
        }
    }
    for key in &keys {
        assert!(
            CONFIG_KEYS.contains(&key.as_str()),
            "the saved document uses the key {key:?}, which is not on the allowlist"
        );
        // The flag is the one key whose name says "secret", and it holds no value; every
        // other key is held to the full list of words that would mean a credential.
        if key == "remember_secret" {
            assert_eq!(
                document["connections"][0]["remember_secret"],
                serde_json::Value::Bool(true),
                "the flag says a credential may be in the system store, and nothing more"
            );
            continue;
        }
        let lower = key.to_lowercase();
        for forbidden in [
            "password",
            "passphrase",
            "secret",
            "token",
            "credential",
            "private",
        ] {
            assert!(
                !lower.contains(forbidden),
                "the saved document has a key that names a secret: {key:?}"
            );
        }
    }
    let stored = document["connections"][0].to_string();
    assert!(
        !stored.contains("alice-password"),
        "no value of a credential is written beside the record"
    );
}

#[test]
fn a_forged_credential_key_is_still_refused_by_the_audit() {
    let dir = TempDir::new("forged-key");
    let store = dir.store();
    let id = HostId::new();
    // The allowlist is not a formality: a file that carries a credential field is not a
    // document this build wrote, and loading it fails instead of quietly accepting it.
    for forged in ["password", "token", "credential"] {
        dir.write_file(&format!(
            r#"{{"version":{FORMAT_VERSION},"hosts":[],"connections":[{{"protocol":"vnc","id":"{id}","name":"wall","address":"10.0.0.7","port":5900,"{forged}":"hunter2"}}],"recents":[]}}"#
        ));
        let error = store
            .load()
            .expect_err("a credential field is not part of a saved connection");
        assert!(
            error.to_string().contains(forged) || error.to_string().contains("unknown field"),
            "the refusal names what it found: {error}"
        );
    }
}

// ---------------------------------------------------------------------------
// Favourites
// ---------------------------------------------------------------------------

#[test]
fn a_favourite_host_and_connection_survive_a_restart_in_the_order_they_were_marked() {
    let dir = TempDir::new("favourites");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let db = saved(&store, host("db", "10.0.0.6", 22, "deploy", None));

    let mut book = store.load().expect("the book loads");
    let console = serial("Console", "COM3");
    book.insert_connection(console.clone())
        .expect("the connection is inserted");
    assert!(book.favorites().is_empty(), "a fresh book has no favourite");
    assert_eq!(book.set_favorite(console.id(), true), Ok(true));
    assert_eq!(book.set_favorite(web.id(), true), Ok(true));
    assert_eq!(
        book.set_favorite(web.id(), true),
        Ok(false),
        "marking a favourite twice changes nothing"
    );
    assert_eq!(book.favorites(), &[console.id(), web.id()]);
    store.save(&book).expect("the book saves");

    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(restarted, book, "the favourites read back exactly as saved");
    assert!(restarted.is_favorite(web.id()));
    assert!(restarted.is_favorite(console.id()));
    assert!(!restarted.is_favorite(db.id()));
    assert_eq!(
        restarted.favorite_hosts(),
        vec![console.clone(), ConnectionRecord::Ssh(web.clone())],
        "both protocols are listed, in the order they were marked"
    );

    let document: serde_json::Value =
        serde_json::from_str(&dir.read_file()).expect("the file is JSON");
    assert_eq!(document["favorites"][0], console.id().to_string());
    assert_eq!(document["favorites"][1], web.id().to_string());
    let mut keys = Vec::new();
    collect_keys(&document, &mut keys);
    for key in &keys {
        assert!(
            CONFIG_KEYS.contains(&key.as_str()),
            "the saved document uses the key {key:?}, which is not on the allowlist"
        );
    }
}

#[test]
fn clearing_and_toggling_a_favourite_updates_the_saved_marks() {
    let dir = TempDir::new("favourite-toggle");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));

    let mut book = store.load().expect("the book loads");
    assert_eq!(book.toggle_favorite(web.id()), Ok(true));
    assert!(book.is_favorite(web.id()));
    assert_eq!(book.toggle_favorite(web.id()), Ok(false));
    assert!(!book.is_favorite(web.id()));
    assert_eq!(
        book.set_favorite(web.id(), false),
        Ok(false),
        "clearing a mark that is not there changes nothing"
    );
    book.set_favorite(web.id(), true).expect("marked");
    store.save(&book).expect("the book saves");
    let mut book = store.load().expect("the book loads");
    assert!(book.is_favorite(web.id()), "the mark survives a restart");
    book.set_favorite(web.id(), false).expect("cleared");
    store.save(&book).expect("the book saves");

    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    assert!(restarted.favorites().is_empty());
}

#[test]
fn a_record_that_is_not_saved_cannot_become_a_favourite() {
    let mut book = HostBook::new();
    let stranger = HostId::new();
    assert_eq!(
        book.set_favorite(stranger, true),
        Err(BookError::UnknownHost { id: stranger })
    );
    assert_eq!(
        book.toggle_favorite(stranger),
        Err(BookError::UnknownHost { id: stranger })
    );
    assert!(book.favorites().is_empty(), "a refused mark leaves nothing");
}

#[test]
fn deleting_a_favourite_host_or_connection_drops_its_mark() {
    let dir = TempDir::new("favourite-delete");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let db = saved(&store, host("db", "10.0.0.6", 22, "deploy", None));
    let mut book = store.load().expect("the book loads");
    let office = rdp("Office", "10.0.0.9", "alice");
    let wall = vnc("Wall", "10.0.0.7");
    book.insert_connection(office.clone()).expect("inserted");
    book.insert_connection(wall.clone()).expect("inserted");
    for id in [web.id(), db.id(), office.id(), wall.id()] {
        book.set_favorite(id, true).expect("marked");
    }

    book.remove(web.id()).expect("the host is removed");
    book.remove_connection(db.id())
        .expect("an SSH host is removed as a connection");
    book.remove_connection(office.id())
        .expect("the desktop connection is removed");
    assert_eq!(book.favorites(), &[wall.id()]);
    store
        .save(&book)
        .expect("a book whose favourites were pruned saves");

    let restarted = HostStore::at(dir.path())
        .load()
        .expect("the file loads, with no mark left pointing at a deleted record");
    assert_eq!(restarted.favorites(), &[wall.id()]);
    assert_eq!(restarted.favorite_hosts(), vec![wall]);
}

#[test]
fn changing_the_protocol_of_a_favourite_keeps_its_mark() {
    let mut book = HostBook::new();
    let web = host("web", "10.0.0.5", 22, "deploy", None);
    book.insert(web.clone()).expect("inserted");
    book.set_favorite(web.id(), true).expect("marked");

    // The same identifier, now a serial console: still the record the user marked.
    let console = serial("web console", "COM3").reidentified(web.id());
    book.update_connection(console.clone(), web.id())
        .expect("the protocol changes");
    assert!(book.is_favorite(web.id()));
    assert_eq!(book.favorite_hosts(), vec![console]);
}

#[test]
fn a_favourite_of_an_unknown_record_or_a_repeated_one_is_refused() {
    let dir = TempDir::new("favourite-invalid");
    let store = dir.store();
    let id = HostId::new();
    let stranger = HostId::new();
    let record = format!(
        r#"{{"id":"{id}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}"#
    );

    let dangling = format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[{record}],"recents":[],"favorites":["{stranger}"]}}"#
    );
    dir.write_file(&dangling);
    let error = store
        .load()
        .expect_err("a favourite of a record that is not saved is refused");
    assert!(
        matches!(&error, StorageError::Corrupt { reason, .. } if reason.contains(&stranger.to_string())),
        "the refusal names the unknown record: {error:?}"
    );
    assert_eq!(dir.read_file(), dangling, "the file is left as it was");

    let repeated = format!(
        r#"{{"version":{FORMAT_VERSION},"hosts":[{record}],"recents":[],"favorites":["{id}","{id}"]}}"#
    );
    dir.write_file(&repeated);
    let error = store
        .load()
        .expect_err("a record marked twice is not a file this build wrote");
    assert!(
        matches!(&error, StorageError::Corrupt { reason, .. } if reason.contains(&id.to_string())),
        "the refusal names the repeated record: {error:?}"
    );
}

#[test]
fn a_book_without_favourites_is_written_byte_for_byte_as_before() {
    // The empty book, exactly as the build before favourites wrote it.
    let dir = TempDir::new("favourite-empty-shape");
    dir.store()
        .save(&HostBook::new())
        .expect("the empty book saves");
    assert_eq!(
        dir.read_file(),
        "{\n  \"version\": 1,\n  \"hosts\": [],\n  \"recents\": []\n}"
    );

    // A populated book: it writes only the keys an older build knew, and saving it
    // again after a mark was set and cleared writes the same bytes.
    let dir = TempDir::new("favourite-legacy-shape");
    let store = dir.store();
    let web = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
    let id = web.id();
    let console = serial("Console", "COM3");
    let mut book = HostBook::new();
    book.insert(web).expect("inserted");
    book.insert_connection(console.clone()).expect("inserted");
    book.record_connection(id, 1_700_000_000).expect("recorded");
    store.save(&book).expect("the book saves");
    let before = dir.read_file();
    let document: serde_json::Value = serde_json::from_str(&before).expect("JSON");
    let keys: Vec<&str> = document
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        document.get("favorites").is_none(),
        "a book without favourites writes no favorites key: {document}"
    );
    assert_eq!(keys.len(), 4, "only the keys an older build knew: {keys:?}");

    let mut book = store.load().expect("the book loads");
    book.set_favorite(id, true).expect("marked");
    book.set_favorite(console.id(), true).expect("marked");
    store.save(&book).expect("the marked book saves");
    assert_ne!(dir.read_file(), before, "the marks are really written");
    let mut book = store.load().expect("the book loads");
    book.set_favorite(id, false).expect("cleared");
    book.set_favorite(console.id(), false).expect("cleared");
    store.save(&book).expect("the cleared book saves");
    assert_eq!(dir.read_file(), before);
}

#[test]
fn an_older_host_file_reads_with_no_favourites() {
    let dir = TempDir::new("favourite-legacy-read");
    let id = HostId::new();
    dir.write_file(&format!(
        r#"{{"version":1,"hosts":[{{"id":"{id}","name":"web","address":"10.0.0.5","port":22,"user":"deploy","group":null}}],"recents":[]}}"#
    ));
    let book = dir.store().load().expect("an older file still loads");
    assert!(book.favorites().is_empty());
    assert!(!book.is_favorite(id));
    assert!(book.favorite_hosts().is_empty());
}

// ---------------------------------------------------------------------------
// Desktop clipboard and mapped folder in backups and sync
// ---------------------------------------------------------------------------

/// An RDP connection that turned the clipboard off and maps `folder`.
fn mapped_rdp(folder: Option<&str>) -> RdpConfig {
    let mut config = RdpConfig::new("Office", "10.0.0.9", Some("alice")).expect("valid");
    config.set_clipboard(false);
    config.set_drive_folder(folder).expect("a usable folder");
    config
}

/// A machine holding exactly `records`.
fn machine(records: impl IntoIterator<Item = ConnectionRecord>) -> LocalState {
    let mut state = LocalState::default();
    for record in records {
        state.hosts.insert_connection(record).expect("inserted");
    }
    state
}

/// The document `state` exports on `platform`, carrying the other platforms'
/// sections of `base`.
fn export_from(
    state: &LocalState,
    platform: Platform,
    base: Option<&ShelluneData>,
) -> ShelluneData {
    let mut options = ExportOptions::new(platform);
    options.created_unix = 1_800_000_000;
    ShelluneData::export_from_vault(state, &options, None, base)
        .expect("exported")
        .document
}

/// The document as JSON.
fn document_json(document: &ShelluneData) -> serde_json::Value {
    serde_json::from_slice(&document.to_json().expect("encodes")).expect("JSON")
}

/// The RDP record with `id` in a local state.
fn rdp_in(state: &LocalState, id: HostId) -> RdpConfig {
    match state.hosts.connection(id) {
        Some(ConnectionRecord::Rdp(config)) => config,
        other => panic!("an RDP record, not {other:?}"),
    }
}

/// The RDP record with `id` after applying `document` to `local` on `platform`.
fn applied_rdp(
    document: &ShelluneData,
    local: &LocalState,
    platform: Platform,
    mode: ApplyMode,
    id: HostId,
) -> RdpConfig {
    let plan = document.plan(local, platform, mode, None).expect("planned");
    assert!(
        plan.fallbacks().iter().all(|fallback| fallback.id != id),
        "a mapped folder is never reported as taken from another machine: {:?}",
        plan.fallbacks()
    );
    rdp_in(plan.result(), id)
}

/// The document read back after `edit` changed its JSON.
fn edited(document: &ShelluneData, edit: impl FnOnce(&mut serde_json::Value)) -> ShelluneData {
    let mut value = document_json(document);
    edit(&mut value);
    ShelluneData::from_json(&serde_json::to_vec(&value).expect("encodes")).expect("reads")
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_mapped_folder_is_kept_in_the_platform_section_and_never_in_the_shared_record() {
    let path = r"C:\Users\alice\Exchange";
    let office = mapped_rdp(Some(path));
    let id = office.id();
    let state = machine([ConnectionRecord::Rdp(office)]);
    let document = export_from(&state, Platform::Windows, None);

    let json = document_json(&document);
    let shared = &json["shared"]["hosts"]["connections"][0];
    assert!(
        shared.get("drive_folder").is_none(),
        "the shared record carries no local path: {shared}"
    );
    assert_eq!(
        shared["clipboard"],
        serde_json::Value::Bool(false),
        "the clipboard choice is shared"
    );
    assert_eq!(
        json["platforms"]["windows"]["hosts"][id.to_string()]["drive_folder"],
        serde_json::json!(path)
    );
    let section = document.platform(Platform::Windows).expect("a section");
    assert_eq!(
        section
            .hosts
            .get(&id)
            .and_then(|overlay| overlay.drive_folder.as_deref()),
        Some(path)
    );
    assert_eq!(
        rdp_in(&state, id).drive_folder(),
        Some(path),
        "exporting leaves the local record as it was"
    );

    // A connection without a folder is still listed, saying this platform maps
    // nothing; VNC has nothing machine-specific and is not listed.
    let plain = RdpConfig::new("Lab", "10.0.0.10", None::<String>).expect("valid");
    let plain_id = plain.id();
    let wall = VncConfig::new("Wall", "10.0.0.7").expect("valid");
    let wall_id = wall.id();
    let document = export_from(
        &machine([ConnectionRecord::Rdp(plain), ConnectionRecord::Vnc(wall)]),
        Platform::Linux,
        None,
    );
    let section = document.platform(Platform::Linux).expect("a section");
    assert_eq!(
        section
            .hosts
            .get(&plain_id)
            .map(|overlay| overlay.drive_folder.clone()),
        Some(None)
    );
    assert!(!section.hosts.contains_key(&wall_id));
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn an_imported_folder_comes_from_this_platform_then_this_machine_and_never_another_platform() {
    let windows_path = r"C:\Users\alice\Exchange";
    let linux_path = "/home/alice/exchange";
    let office = mapped_rdp(Some(windows_path));
    let id = office.id();
    let windows = machine([ConnectionRecord::Rdp(office.clone())]);
    let document = export_from(&windows, Platform::Windows, None);
    let empty = LocalState::default();

    // Another Windows machine takes the folder from the Windows section, and the
    // clipboard choice arrives with the record.
    let arrived = applied_rdp(&document, &empty, Platform::Windows, ApplyMode::Merge, id);
    assert_eq!(arrived.drive_folder(), Some(windows_path));
    assert!(!arrived.clipboard());

    // A Linux machine without the record maps nothing.
    let arrived = applied_rdp(&document, &empty, Platform::Linux, ApplyMode::Replace, id);
    assert_eq!(arrived.drive_folder(), None);
    assert!(!arrived.clipboard());

    // A Linux machine with its own folder for the record keeps it.
    let mut mine = office.clone();
    mine.set_drive_folder(Some(linux_path)).expect("usable");
    let linux = machine([ConnectionRecord::Rdp(mine)]);
    for mode in [ApplyMode::Merge, ApplyMode::Replace] {
        let arrived = applied_rdp(&document, &linux, Platform::Linux, mode, id);
        assert_eq!(arrived.drive_folder(), Some(linux_path));
    }

    // Once Linux has written its own section, each platform finds its own path in
    // the same document.
    let both = export_from(&linux, Platform::Linux, Some(&document));
    for (platform, path) in [
        (Platform::Windows, windows_path),
        (Platform::Linux, linux_path),
    ] {
        let arrived = applied_rdp(&both, &empty, platform, ApplyMode::Replace, id);
        assert_eq!(arrived.drive_folder(), Some(path));
    }

    // The platform section wins over this machine's own value, including when it
    // says the folder was turned off.
    let mut cleared = office;
    cleared.set_drive_folder(None::<String>).expect("cleared");
    let turned_off = export_from(
        &machine([ConnectionRecord::Rdp(cleared)]),
        Platform::Windows,
        None,
    );
    let arrived = applied_rdp(
        &turned_off,
        &windows,
        Platform::Windows,
        ApplyMode::Merge,
        id,
    );
    assert_eq!(arrived.drive_folder(), None);

    // A folder written into the shared record by hand is never used.
    let forged = edited(&document, |value| {
        value["shared"]["hosts"]["connections"][0]["drive_folder"] =
            serde_json::json!(windows_path);
        value["platforms"] = serde_json::json!({});
    });
    for platform in [Platform::Windows, Platform::Linux] {
        let arrived = applied_rdp(&forged, &empty, platform, ApplyMode::Replace, id);
        assert_eq!(arrived.drive_folder(), None);
    }
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn the_sync_view_of_a_platform_changes_with_its_own_folder_only() {
    let office = mapped_rdp(Some("/home/alice/exchange"));
    let id = office.id();
    let before = export_from(
        &machine([ConnectionRecord::Rdp(office.clone())]),
        Platform::Linux,
        None,
    );
    let mut moved = office.clone();
    moved
        .set_drive_folder(Some("/srv/exchange"))
        .expect("usable");
    let after = export_from(
        &machine([ConnectionRecord::Rdp(moved)]),
        Platform::Linux,
        Some(&before),
    );
    assert_ne!(
        before.sync_view(Platform::Linux).expect("view"),
        after.sync_view(Platform::Linux).expect("view"),
        "a new folder is a change on this platform"
    );
    assert_ne!(
        before.core_view(Platform::Linux).expect("view"),
        after.core_view(Platform::Linux).expect("view")
    );
    assert_eq!(
        before.sync_view(Platform::Windows).expect("view"),
        after.sync_view(Platform::Windows).expect("view"),
        "another platform sees no change"
    );

    // The clipboard choice is shared, so every platform sees it change.
    let mut shared = office;
    shared.set_clipboard(true);
    let toggled = export_from(
        &machine([ConnectionRecord::Rdp(shared)]),
        Platform::Linux,
        None,
    );
    assert_ne!(
        before.sync_view(Platform::Windows).expect("view"),
        toggled.sync_view(Platform::Windows).expect("view")
    );
    let arrived = applied_rdp(
        &toggled,
        &LocalState::default(),
        Platform::Windows,
        ApplyMode::Replace,
        id,
    );
    assert!(arrived.clipboard());
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn an_edit_to_the_folder_survives_a_rebase_over_another_writer() {
    let office = mapped_rdp(None);
    let id = office.id();
    let base = machine([ConnectionRecord::Rdp(office.clone())]);
    let mut ours = base.clone();
    let mut mapped = office.clone();
    mapped
        .set_drive_folder(Some("D:/exchange"))
        .expect("usable");
    ours.hosts
        .update_connection(ConnectionRecord::Rdp(mapped), id)
        .expect("updated");
    let mut theirs = base.clone();
    let mut renamed = office;
    renamed.set_name("Office 2").expect("valid");
    renamed.set_clipboard(true);
    theirs
        .hosts
        .update_connection(ConnectionRecord::Rdp(renamed), id)
        .expect("updated");

    let touched = Touched {
        hosts: true,
        ..Touched::default()
    };
    let (merged, dropped) = rebase_state(&base, &ours, &theirs, touched);
    assert!(dropped.is_empty(), "{dropped:?}");
    let merged = rdp_in(&merged, id);
    assert_eq!(
        merged.drive_folder(),
        Some("D:/exchange"),
        "our edit is kept"
    );
    assert_eq!(merged.name(), "Office 2", "and so is theirs");
    assert!(merged.clipboard());
}

#[test]
#[ignore = "opt-in RDP/VNC test"]
fn a_platform_section_with_an_unusable_folder_is_refused() {
    let office = mapped_rdp(Some("/home/alice/exchange"));
    let id = office.id();
    let document = export_from(
        &machine([ConnectionRecord::Rdp(office)]),
        Platform::Linux,
        None,
    );
    let mut value = document_json(&document);
    value["platforms"]["linux"]["hosts"][id.to_string()]["drive_folder"] =
        serde_json::json!("/home/alice/\u{7}");
    let error = ShelluneData::from_json(&serde_json::to_vec(&value).expect("encodes"))
        .expect_err("a control character is not a folder");
    assert!(
        matches!(&error, PortableError::Corrupt { reason } if reason.contains(&id.to_string())),
        "{error}"
    );
}

#[test]
fn a_document_with_a_record_this_build_cannot_read_still_applies_and_keeps_it() {
    let lab = RdpConfig::new("Lab", "10.0.0.10", None::<String>).expect("valid");
    let lab_id = lab.id();
    let future_id = HostId::new();
    let document = export_from(
        &machine([ConnectionRecord::Rdp(lab)]),
        Platform::Windows,
        None,
    );
    let future = serde_json::json!({
        "protocol": "rdp",
        "id": future_id,
        "name": "Office",
        "address": "10.0.0.9",
        "port": 3389,
        "a_later_option": true
    });
    let document = edited(&document, |value| {
        value["shared"]["hosts"]["connections"]
            .as_array_mut()
            .expect("a list")
            .push(future.clone());
        value["shared"]["hosts"]["favorites"] = serde_json::json!([future_id]);
    });
    let [unreadable] = document.shared().hosts().unreadable_connections() else {
        panic!("the record is reported");
    };
    assert_eq!(unreadable.id(), Some(future_id));

    // A replacement carries the record, and exporting again writes it unchanged.
    let plan = document
        .plan(
            &LocalState::default(),
            Platform::Windows,
            ApplyMode::Replace,
            None,
        )
        .expect("one unreadable record does not stop the import");
    let result = plan.result().clone();
    assert!(result.hosts.connection(lab_id).is_some());
    assert_eq!(result.hosts.unreadable_connections().len(), 1);
    assert!(
        result.hosts.is_favorite(future_id),
        "its mark stays with it"
    );
    let again = document_json(&export_from(&result, Platform::Windows, None));
    assert!(
        again["shared"]["hosts"]["connections"]
            .as_array()
            .expect("a list")
            .contains(&future)
    );

    // A merge keeps a record set aside here when the document does not carry it.
    let other = export_from(&LocalState::default(), Platform::Windows, None);
    let plan = other
        .plan(&result, Platform::Windows, ApplyMode::Merge, None)
        .expect("planned");
    assert_eq!(plan.result().hosts.unreadable_connections().len(), 1);

    // A readable record with the same identifier replaces the one set aside.
    let readable =
        ConnectionRecord::Rdp(RdpConfig::new("Office", "10.0.0.9", None::<String>).expect("valid"))
            .reidentified(future_id);
    let upgraded = export_from(&machine([readable]), Platform::Windows, None);
    let plan = upgraded
        .plan(&result, Platform::Windows, ApplyMode::Merge, None)
        .expect("planned");
    assert!(plan.result().hosts.unreadable_connections().is_empty());
    assert!(plan.result().hosts.connection(future_id).is_some());
}

#[test]
fn a_record_that_names_a_credential_still_fails_the_whole_document() {
    let document = export_from(&LocalState::default(), Platform::Windows, None);
    let mut value = document_json(&document);
    value["shared"]["hosts"]["connections"] = serde_json::json!([{
        "protocol": "telnet",
        "id": HostId::new(),
        "name": "switch",
        "password": "hunter2"
    }]);
    let error = ShelluneData::from_json(&serde_json::to_vec(&value).expect("encodes"))
        .expect_err("a credential is never kept");
    assert!(
        matches!(&error, PortableError::Corrupt { reason } if reason.contains("password") && !reason.contains("hunter2")),
        "{error}"
    );
}

#[test]
fn recent_connections_record_list_persist_and_clean_up_across_all_protocols() {
    let dir = TempDir::new("recent-all-protocols");
    let store = dir.store();
    let web = saved(&store, host("web", "10.0.0.5", 22, "deploy", None));
    let mut book = store.load().expect("the book loads");
    let office = rdp("Office", "10.0.0.9", "alice");
    let wall = vnc("Wall", "10.0.0.7");
    let console = serial("Console", "COM3");
    book.insert_connection(office.clone())
        .expect("rdp inserted");
    book.insert_connection(wall.clone()).expect("vnc inserted");
    book.insert_connection(console.clone())
        .expect("serial inserted");

    book.record_connection(web.id(), 1_700_000_100)
        .expect("ssh recorded");
    book.record_connection(office.id(), 1_700_000_300)
        .expect("rdp recorded");
    book.record_connection(wall.id(), 1_700_000_200)
        .expect("vnc recorded");
    book.record_connection(console.id(), 1_700_000_400)
        .expect("serial recorded");
    book.record_connection(office.id(), 1_700_000_500)
        .expect("rdp recorded twice");

    assert_eq!(
        book.version(),
        shellune_storage::FORMAT_VERSION,
        "format version remains v1"
    );
    store.save(&book).expect("the book saves");

    let mut reloaded = dir.store().load().expect("reloaded cleanly");
    assert_eq!(reloaded.version(), 1);
    let ordered: Vec<(HostId, u64, u32)> = reloaded
        .recent_records()
        .into_iter()
        .map(|(record, at, count)| (record.id(), at, count))
        .collect();
    assert_eq!(
        ordered,
        vec![
            (office.id(), 1_700_000_500, 2),
            (console.id(), 1_700_000_400, 1),
            (wall.id(), 1_700_000_200, 1),
            (web.id(), 1_700_000_100, 1),
        ],
        "recent_records orders SSH, RDP, VNC and Serial newest first"
    );

    reloaded
        .remove_connection(office.id())
        .expect("rdp removed");
    reloaded
        .remove_connection(console.id())
        .expect("serial removed");
    assert!(reloaded.recent_of(office.id()).is_none());
    assert!(reloaded.recent_of(console.id()).is_none());
    store
        .save(&reloaded)
        .expect("saving after removing non-SSH recent records keeps invariants valid");
    let after_remove = dir.store().load().expect("reloads after removal");
    let remaining: Vec<HostId> = after_remove
        .recent_records()
        .into_iter()
        .map(|(record, _, _)| record.id())
        .collect();
    assert_eq!(remaining, vec![wall.id(), web.id()]);
}

// ---------------------------------------------------------------------------
// Remembered passwords live in the vault (R1, R4)
// ---------------------------------------------------------------------------

/// A version 1 backup of 300 hosts whose passwords the exporting machine's vault
/// held is imported into an empty machine: every password lands in the new
/// machine's vault, saved once with the files, and the credential store is only
/// asked for the vault's own unseal secret - no `host-<uuid>` entry is read or
/// written on either side.
#[test]
fn importing_three_hundred_remembered_passwords_fills_the_vault_and_no_host_entry() {
    let kdf = KdfParams::new(8, 1, 1).expect("cheap parameters");
    let mut state = LocalState::default();
    let mut exporter = VaultContent::new();
    let mut ids = Vec::new();
    for index in 0..300_u32 {
        let mut record = host(
            &format!("host-{index}"),
            &format!("10.1.{}.{}", index / 200, index % 200 + 1),
            22,
            "deploy",
            None,
        );
        record.set_auth(Some(AuthMethodKind::Password));
        record.set_remember_secret(true);
        exporter.set_host_secret(
            record.id(),
            SecretString::new(format!("password-{index}")),
            "exporter",
        );
        ids.push(record.id());
        state
            .hosts
            .apply(&HostEdit::create(record))
            .expect("the host is inserted");
    }
    let mut options =
        ExportOptions::new(Platform::current()).with_passphrase(SecretString::new("bundle"));
    options.kdf = kdf;
    let outcome =
        ShelluneData::export_from_vault(&state, &options, Some(&exporter), None).expect("exported");
    assert!(
        outcome.secrets_missing.is_empty(),
        "the vault held every one"
    );
    let bundle = outcome
        .document
        .open_secrets(&SecretString::new("bundle"))
        .expect("the passphrase opens the block");

    let dir = TempDir::new("vault-import-300");
    let data = shellune_storage::LocalData::at(dir.path());
    let local = data.load().expect("an empty machine loads");
    let plan = outcome
        .document
        .plan(&local, Platform::current(), ApplyMode::Merge, Some(&bundle))
        .expect("planned");
    let records = plan.result().hosts.records();
    let secrets = MemorySecrets::new();
    let store = VaultStore::at(dir.path());
    let mut content = VaultContent::new();
    let mut saves = 0;
    plan.apply_with_vault(&data, &mut content, "importer", "import", &mut |vault| {
        saves += 1;
        store
            .save_records(
                vault,
                &records,
                &[UnsealSource::LocalMachine],
                kdf,
                &secrets,
            )
            .map_err(|error| error.to_string())
    })
    .expect("applied");
    assert_eq!(saves, 1, "the 300 passwords are one vault save");

    let opened = store
        .open(&UnsealSource::LocalMachine, &secrets)
        .expect("the vault opens");
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            opened.host_secret(*id).map(SecretString::expose),
            Some(format!("password-{index}").as_str())
        );
    }
    let accesses = secrets.accesses();
    assert!(
        accesses
            .iter()
            .all(|access| access.user == "vault-local-unseal"),
        "only the vault's unseal secret was asked for: {accesses:?}"
    );
    assert!(secrets.is_empty(), "no host-<uuid> entry exists");
}
