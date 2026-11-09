//! Saved commands, as they are written and read back (R6).
//!
//! A restart is a second store on the same directory. These tests fail if a damaged or
//! newer library is treated as an empty one, if a saved record grows a field that could
//! hold a credential, or if loading a library starts anything: the store has no channel
//! to a terminal, and the document is read as data only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use shellune_core::{SnippetBook, SnippetId, SnippetProblem};
use shellune_storage::{PersistError, SNIPPETS_FILE_NAME, SnippetStore};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "shellune-snippets-{}-{}-{}",
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

    fn store(&self) -> SnippetStore {
        SnippetStore::at(&self.path)
    }

    fn file(&self) -> PathBuf {
        self.path.join(SNIPPETS_FILE_NAME)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The document a previous run would have written, read as JSON for the assertions that
/// check what a file really holds.
fn document(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("the library file is readable");
    serde_json::from_str(&text).expect("the library file is JSON")
}

#[test]
fn a_library_survives_a_restart_with_its_names_bodies_and_order() {
    let dir = TempDir::new("restart");
    let mut book = SnippetBook::new();
    let deploy = book
        .create("deploy", "make deploy && obsctl status")
        .expect("a valid command");
    let checks = book
        .create("checks", "cargo fmt --all\ncargo test --workspace")
        .expect("a valid multi-line command");
    dir.store().save(&book).expect("the library saves");

    // The restart: a new store on the same directory, reading what the last run wrote.
    let restarted = SnippetStore::at(dir.path());
    let read = restarted.load().expect("the library loads");
    let names: Vec<&str> = read.snippets().iter().map(|s| s.name()).collect();
    assert_eq!(names, ["deploy", "checks"]);
    assert_eq!(read.snippets()[0].id(), deploy);
    assert_eq!(read.snippets()[1].id(), checks);
    assert_eq!(
        read.snippets()[1].body(),
        "cargo fmt --all\ncargo test --workspace",
        "a multi-line body is kept exactly, so a line break cannot become a space"
    );
    assert!(read.snippets()[1].is_multiline());
}

#[test]
fn an_edited_library_keeps_the_ids_of_the_records_it_did_not_touch() {
    let dir = TempDir::new("edit");
    let mut book = SnippetBook::new();
    let keep = book.create("keep", "echo keep").expect("a valid command");
    let change = book.create("change", "echo one").expect("a valid command");
    dir.store().save(&book).expect("the library saves");

    let mut read = dir.store().load().expect("the library loads");
    read.replace(change, "changed", "echo two").expect("edited");
    read.remove(keep).expect("removed");
    dir.store().save(&read).expect("the library saves again");

    let again = dir.store().load().expect("the library loads");
    assert_eq!(
        again
            .snippets()
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>(),
        ["changed"]
    );
    assert_eq!(again.snippets()[0].id(), change);
    assert_eq!(again.snippets()[0].body(), "echo two");
}

#[test]
fn a_written_library_holds_no_credential_field_and_no_extra_record() {
    let dir = TempDir::new("shape");
    let mut book = SnippetBook::new();
    book.create("deploy", "ssh deploy@host 'systemctl status app'")
        .expect("a valid command");
    dir.store().save(&book).expect("the library saves");

    let value = document(&dir.file());
    assert_eq!(value["version"], 1);
    let records = value["snippets"]
        .as_array()
        .expect("the library holds a list of commands");
    assert_eq!(records.len(), 1);
    let record = records[0]
        .as_object()
        .expect("a record is an object, not a bare string");
    let mut keys: Vec<&str> = record.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["body", "id", "name"],
        "a record carries exactly the id, the name and the command body"
    );
    let text = std::fs::read_to_string(dir.file()).expect("the file is readable");
    for forbidden in ["password", "passphrase", "secret", "token", "private_key"] {
        assert!(
            !text.to_ascii_lowercase().contains(forbidden),
            "the file holds no {forbidden}: {text}"
        );
    }
}

#[test]
fn a_damaged_library_is_reported_and_never_replaced_by_an_empty_one() {
    let dir = TempDir::new("damaged");
    let broken = b"{\"version\":1,\"snippets\":[{\"id\":";
    std::fs::write(dir.file(), broken).expect("the damaged file is written");

    let error = dir.store().load().expect_err("damage is a failure");
    assert!(
        matches!(error, PersistError::Corrupt { .. }),
        "the reason names the file as damaged: {error:?}"
    );
    assert_eq!(
        std::fs::read(dir.file()).expect("the file is still there"),
        broken.to_vec(),
        "reading a damaged library does not overwrite it, so a save can be refused"
    );
    assert!(
        error.to_string().contains(SNIPPETS_FILE_NAME),
        "the reason names the file to repair: {error}"
    );
}

#[test]
fn a_library_from_a_newer_build_is_named_as_such() {
    let dir = TempDir::new("newer");
    std::fs::write(
        dir.file(),
        br#"{"version":4,"snippets":[{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"x","body":"ls","later":true}]}"#,
    )
    .expect("the newer file is written");
    let error = dir.store().load().expect_err("a newer format is refused");
    assert!(
        matches!(error, PersistError::UnsupportedVersion { found: 4, .. }),
        "a newer library is reported as newer, not as damage: {error:?}"
    );
}

#[test]
fn a_field_that_names_a_secret_is_refused_and_its_value_is_not_echoed() {
    let dir = TempDir::new("secret");
    std::fs::write(
        dir.file(),
        br#"{"version":1,"snippets":[{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"login","body":"ssh me@host","password":"hunter2"}]}"#,
    )
    .expect("the file is written");
    let error = dir
        .store()
        .load()
        .expect_err("a credential field is refused");
    let reason = error.to_string();
    assert!(reason.contains("password"), "{reason}");
    assert!(
        !reason.contains("hunter2"),
        "the refused value never reaches a message: {reason}"
    );
}

#[test]
fn a_record_this_build_would_refuse_is_not_loaded() {
    let dir = TempDir::new("refused");
    // A record with no command body would insert nothing, so it is not a library this
    // build could have written, and it is reported instead of being skipped quietly.
    std::fs::write(
        dir.file(),
        br#"{"version":1,"snippets":[{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"empty","body":""}]}"#,
    )
    .expect("the file is written");
    assert!(dir.store().load().is_err());
}

#[test]
fn a_first_run_has_no_library_and_writes_no_file() {
    let dir = TempDir::new("first-run");
    let book = dir.store().load().expect("a missing file is a first run");
    assert!(book.is_empty());
    assert!(
        !dir.file().exists(),
        "reading a library never creates the file"
    );
}

#[test]
fn a_removed_record_cannot_be_used_again_by_its_old_id() {
    let dir = TempDir::new("removed");
    let mut book = SnippetBook::new();
    let id: SnippetId = book.create("gone", "echo gone").expect("a valid command");
    book.remove(id);
    dir.store().save(&book).expect("the empty library saves");
    let read = dir.store().load().expect("the library loads");
    assert!(read.is_empty());
    assert!(read.get(id).is_none());
    let mut edited = read;
    assert_eq!(
        edited.replace(id, "back", "echo back"),
        Err(SnippetProblem::Unknown { id })
    );
}
