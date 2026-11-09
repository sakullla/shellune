//! The file the saved commands live in.
//!
//! The store reads and writes the library. Loading it starts nothing: no pane, no
//! shell and no command. A newer or damaged file is reported and left exactly as it
//! was, rather than replaced with an empty library — the caller decides whether to
//! keep a broken file for repair or to save something else over it, and a store that
//! quietly answered "no snippets" would make replacing it look like a normal save.
//!
//! The file holds names and command bodies only. A document with a field that names a
//! credential is refused by the shared versioned reader, so a hand-edited library
//! cannot turn this file into a place passwords are kept.

use std::path::{Path, PathBuf};

use shellune_core::{SNIPPETS_FORMAT_VERSION, SnippetBook};

use crate::error::PersistError;
use crate::store::{load_versioned, save_versioned};

/// Name of the snippet file inside the data directory.
pub const FILE_NAME: &str = "snippets.json";

const LABEL: &str = "saved commands";

/// A snippet library at a known location.
#[derive(Clone, Debug)]
pub struct SnippetStore {
    root: PathBuf,
}

impl SnippetStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A store beside the hosts, the workspace and the preferences.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(crate::store::data_directory())
    }

    /// The directory the library lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The library file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// Reads the library.
    ///
    /// A missing file is a first run and yields an empty library. Reading does not
    /// insert anything into a terminal and does not start a process.
    ///
    /// # Errors
    ///
    /// Returns the read failure, the corruption (a record this build would refuse, a
    /// repeated id, a credential-shaped field) or the unsupported-version failure. The
    /// file is left as it was.
    pub fn load(&self) -> Result<SnippetBook, PersistError> {
        load_versioned(
            &self.root,
            FILE_NAME,
            SNIPPETS_FORMAT_VERSION,
            LABEL,
            |book: &SnippetBook| book.validate(),
        )
    }

    /// Writes the library atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write, replace or encode failure. The previous file stays
    /// in place, so a failed save never leaves half a library behind.
    pub fn save(&self, book: &SnippetBook) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(book).map_err(|error| PersistError::Encode {
            label: LABEL,
            reason: error.to_string(),
        })?;
        save_versioned(&self.root, FILE_NAME, LABEL, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shellune_core::SnippetId;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shellune-snippets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_is_a_first_run_with_no_library() {
        let store = SnippetStore::at(scratch("missing"));
        let book = store.load().expect("a missing file is not a failure");
        assert!(book.is_empty());
        assert_eq!(book.version(), SNIPPETS_FORMAT_VERSION);
    }

    #[test]
    fn a_saved_library_loads_back_with_its_names_bodies_and_order() {
        let dir = scratch("round-trip");
        let store = SnippetStore::at(&dir);
        let mut book = SnippetBook::new();
        let first = book.create("deploy", "make deploy").expect("created");
        let body = "cargo fmt\ncargo test --workspace\ncargo clippy";
        let second = book.create("checks", body).expect("created");
        store.save(&book).expect("the library saves");

        let read = store.load().expect("the library loads");
        assert_eq!(read, book);
        assert_eq!(read.snippets()[0].id(), first);
        assert_eq!(read.snippets()[1].id(), second);
        assert_eq!(read.snippets()[1].body(), body);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_is_reported_and_left_untouched() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SnippetStore::at(&dir);
        std::fs::write(store.path(), b"{not json").expect("write");
        assert!(
            store.load().is_err(),
            "damage is reported, not read as empty"
        );
        assert_eq!(
            std::fs::read(store.path()).expect("the file is still there"),
            b"{not json".to_vec(),
            "a failed load does not rewrite the file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_library_from_a_newer_build_is_named_as_such_and_not_overwritten() {
        let dir = scratch("newer");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SnippetStore::at(&dir);
        let document = r#"{"version":7,"snippets":[]}"#;
        std::fs::write(store.path(), document).expect("write");
        let error = store.load().expect_err("a newer format is not read");
        let reason = error.to_string();
        assert!(
            reason.contains("format 7") || reason.contains('7'),
            "{reason}"
        );
        assert_eq!(
            std::fs::read_to_string(store.path()).expect("the file is still there"),
            document
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_field_that_names_a_secret_is_refused_without_keeping_its_value() {
        let dir = scratch("secret");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SnippetStore::at(&dir);
        std::fs::write(
            store.path(),
            br#"{"version":1,"snippets":[{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"ssh","body":"ssh me@host","password":"hunter2"}]}"#,
        )
        .expect("write");
        let error = store.load().expect_err("a credential field is refused");
        let reason = error.to_string();
        assert!(reason.contains("password"), "{reason}");
        assert!(
            !reason.contains("hunter2"),
            "the refused value is not copied into the reason: {reason}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_saved_record_has_no_credential_shaped_field() {
        let dir = scratch("shape");
        let store = SnippetStore::at(&dir);
        let mut book = SnippetBook::new();
        book.create("deploy", "make deploy").expect("created");
        store.save(&book).expect("the library saves");
        let text = std::fs::read_to_string(store.path()).expect("the file is there");
        for key in ["password", "passphrase", "secret", "token", "private_key"] {
            assert!(
                !text.to_ascii_lowercase().contains(key),
                "the written library holds no {key} field: {text}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_library_holding_a_record_the_editor_would_refuse_is_reported() {
        let dir = scratch("refused-record");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SnippetStore::at(&dir);
        std::fs::write(
            store.path(),
            br#"{"version":1,"snippets":[{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":" ","body":"ls"}]}"#,
        )
        .expect("write");
        assert!(store.load().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_a_library_that_never_changed_keeps_one_file() {
        let dir = scratch("atomic");
        let store = SnippetStore::at(&dir);
        let mut book = SnippetBook::new();
        let id: SnippetId = book.create("one", "echo one").expect("created");
        store.save(&book).expect("saves once");
        store.save(&book).expect("saves twice");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("the directory is there")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != FILE_NAME)
            .collect();
        assert!(
            leftovers.is_empty(),
            "an atomic save leaves no temporary file behind: {leftovers:?}"
        );
        assert_eq!(store.load().expect("loads").snippets()[0].id(), id);
        let _ = std::fs::remove_dir_all(dir);
    }
}
