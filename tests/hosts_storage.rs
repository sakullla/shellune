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

use shellune_core::{HostConfig, HostId, HostValidationError, JumpFault, Port};
use shellune_storage::{
    BookError, CONFIG_KEYS, FORMAT_VERSION, HostBook, HostEdit, HostStore, LOG_FIELDS, StorageError,
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
