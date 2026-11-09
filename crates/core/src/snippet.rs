//! Saved commands: a name and the text of a command (R6).
//!
//! A snippet is a record, not an action. Loading a library starts nothing, types
//! nothing and sends nothing to a terminal; only the window can offer a body to the
//! active pane, and it always goes through that pane's own confirmation path. The
//! model deliberately has no field for a password, a passphrase, a private key or a
//! token: a shortcut command is ordinary local text, and the editor says so.
//!
//! The file is versioned so a library written by a newer build is reported instead of
//! being half-read, and every record is checked on the way in: a name or a body that
//! this build would refuse to create is also refused when it is read.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The format this build writes for a snippet library.
pub const SNIPPETS_FORMAT_VERSION: u32 = 1;

/// The longest name a snippet can have, in characters.
///
/// The list, the palette and the editor all draw the name on one line, so the limit
/// keeps a name readable there; a longer name is refused with its reason rather than
/// quietly shortened, because a shortened name is not the name the user typed.
pub const MAX_SNIPPET_NAME_CHARS: usize = 64;

/// The longest body a snippet can hold, in characters.
///
/// Multi-line bodies are expected — a snippet is the place a repeated command line or
/// short block goes — so the limit is generous; it exists so one record cannot make the
/// library file unbounded.
pub const MAX_SNIPPET_BODY_CHARS: usize = 8_000;

/// Why a snippet cannot be created, changed or read.
///
/// Each variant is a sentence the editor can show, because "invalid command" does not
/// tell a user what to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetProblem {
    /// The name is empty or only spaces.
    EmptyName,
    /// The name is longer than [`MAX_SNIPPET_NAME_CHARS`] characters.
    NameTooLong {
        /// The limit that applies.
        max: usize,
    },
    /// The body is empty or only spaces.
    EmptyBody,
    /// The body is longer than [`MAX_SNIPPET_BODY_CHARS`] characters.
    BodyTooLong {
        /// The limit that applies.
        max: usize,
    },
    /// The record being changed is not in the library.
    Unknown {
        /// The identifier that was asked for.
        id: SnippetId,
    },
}

impl fmt::Display for SnippetProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(formatter, "a shortcut command needs a name"),
            Self::NameTooLong { max } => write!(
                formatter,
                "the name is longer than {max} characters; shorten it and save again"
            ),
            Self::EmptyBody => write!(
                formatter,
                "a shortcut command needs a command body; nothing would be inserted or run"
            ),
            Self::BodyTooLong { max } => write!(
                formatter,
                "the command body is longer than {max} characters; shorten it and save again"
            ),
            Self::Unknown { id } => write!(
                formatter,
                "the shortcut command {id} is not in this library any more; reload the list"
            ),
        }
    }
}

/// Identifies one saved command.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnippetId(Uuid);

impl SnippetId {
    /// Creates a fresh random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an identifier that was persisted earlier.
    #[must_use]
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Returns the underlying value, for storage keys and logging.
    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for SnippetId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SnippetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Debug for SnippetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SnippetId({})", self.0)
    }
}

/// One saved command: a stable id, a name and the text to insert or run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    id: SnippetId,
    name: String,
    body: String,
}

impl Snippet {
    /// A record with a fresh id.
    ///
    /// # Errors
    ///
    /// Returns [`SnippetProblem`] when the name or the body is empty, only spaces, or
    /// past its limit.
    pub fn new(name: impl Into<String>, body: impl Into<String>) -> Result<Self, SnippetProblem> {
        let name = name.into();
        let body = body.into();
        check_name(&name)?;
        check_body(&body)?;
        Ok(Self {
            id: SnippetId::new(),
            name: name.trim().to_owned(),
            body,
        })
    }

    /// The identifier this record keeps for its whole life, in the file and in the
    /// palette, so a rename never becomes a different entry.
    #[must_use]
    pub fn id(&self) -> SnippetId {
        self.id
    }

    /// The name shown in the list, the palette and the editor.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The text offered to the terminal: what "insert" writes and what "run" runs.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// How many lines the body has, counted the way a terminal would run it.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.body.lines().count().max(1)
    }

    /// Whether the body would need the terminal's paste confirmation.
    ///
    /// The decision itself belongs to the terminal, which owns the rule; this only says
    /// whether the body is more than one line, which is what the list and the editor
    /// mark so a user knows a confirmation will appear before anything is sent.
    #[must_use]
    pub fn is_multiline(&self) -> bool {
        self.body.contains(['\n', '\r'])
    }
}

/// Checks a name a user typed or a file holds.
///
/// # Errors
///
/// Returns [`SnippetProblem::EmptyName`] or [`SnippetProblem::NameTooLong`].
pub fn check_name(name: &str) -> Result<(), SnippetProblem> {
    if name.trim().is_empty() {
        return Err(SnippetProblem::EmptyName);
    }
    if name.trim().chars().count() > MAX_SNIPPET_NAME_CHARS {
        return Err(SnippetProblem::NameTooLong {
            max: MAX_SNIPPET_NAME_CHARS,
        });
    }
    Ok(())
}

/// Checks a body a user typed or a file holds.
///
/// # Errors
///
/// Returns [`SnippetProblem::EmptyBody`] or [`SnippetProblem::BodyTooLong`].
pub fn check_body(body: &str) -> Result<(), SnippetProblem> {
    if body.trim().is_empty() {
        return Err(SnippetProblem::EmptyBody);
    }
    if body.chars().count() > MAX_SNIPPET_BODY_CHARS {
        return Err(SnippetProblem::BodyTooLong {
            max: MAX_SNIPPET_BODY_CHARS,
        });
    }
    Ok(())
}

/// A versioned library of saved commands.
///
/// The book is the file's content and nothing else: it holds no selection, no draft and
/// no pointer, so what is written is exactly what was saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnippetBook {
    version: u32,
    #[serde(default)]
    snippets: Vec<Snippet>,
}

impl Default for SnippetBook {
    fn default() -> Self {
        Self::new()
    }
}

impl SnippetBook {
    /// An empty library at the current format version.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: SNIPPETS_FORMAT_VERSION,
            snippets: Vec::new(),
        }
    }

    /// Format version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Every record, in the order the list shows them.
    #[must_use]
    pub fn snippets(&self) -> &[Snippet] {
        &self.snippets
    }

    /// How many records the library holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.snippets.len()
    }

    /// Whether the library holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    /// One record by id.
    #[must_use]
    pub fn get(&self, id: SnippetId) -> Option<&Snippet> {
        self.snippets.iter().find(|snippet| snippet.id == id)
    }

    /// Adds a record and returns its id.
    ///
    /// Two records may share a name: the id is what the palette and the list act on, so
    /// a duplicate name is a reading problem rather than a refusal.
    ///
    /// # Errors
    ///
    /// Returns [`SnippetProblem`] when the name or the body is unusable.
    pub fn create(
        &mut self,
        name: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<SnippetId, SnippetProblem> {
        let snippet = Snippet::new(name, body)?;
        let id = snippet.id;
        self.snippets.push(snippet);
        Ok(id)
    }

    /// Replaces the name and the body of one record, keeping its id.
    ///
    /// # Errors
    ///
    /// Returns [`SnippetProblem`] when the record is gone or the new name or body is
    /// unusable. Nothing is changed in that case.
    pub fn replace(
        &mut self,
        id: SnippetId,
        name: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<(), SnippetProblem> {
        let name = name.into();
        let body = body.into();
        check_name(&name)?;
        check_body(&body)?;
        let Some(snippet) = self.snippets.iter_mut().find(|snippet| snippet.id == id) else {
            return Err(SnippetProblem::Unknown { id });
        };
        snippet.name = name.trim().to_owned();
        snippet.body = body;
        Ok(())
    }

    /// Removes one record, returning what was removed.
    pub fn remove(&mut self, id: SnippetId) -> Option<Snippet> {
        let index = self.snippets.iter().position(|snippet| snippet.id == id)?;
        Some(self.snippets.remove(index))
    }

    /// Checks the document read from the file.
    ///
    /// A record this build would refuse to create is refused here too, so the library on
    /// screen is always a library the editor could have produced.
    ///
    /// # Errors
    ///
    /// Returns the reason the document cannot be used: a version this build does not
    /// write, a repeated id, or a record with an unusable name or body.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != SNIPPETS_FORMAT_VERSION {
            return Err(format!(
                "format version {} is not the version this build writes ({SNIPPETS_FORMAT_VERSION}). The library was not loaded and nothing was overwritten.",
                self.version
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for snippet in &self.snippets {
            check_name(&snippet.name).map_err(|problem| {
                format!(
                    "the saved command {} has an unusable name: {problem}",
                    snippet.id
                )
            })?;
            check_body(&snippet.body).map_err(|problem| {
                format!(
                    "the saved command {} has an unusable body: {problem}",
                    snippet.id
                )
            })?;
            if !seen.insert(snippet.id) {
                return Err(format!(
                    "two saved commands share the id {}; the library was not loaded",
                    snippet.id
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_record_keeps_its_name_and_body() {
        let mut book = SnippetBook::new();
        let id = book
            .create(
                "  deploy  ",
                "cargo build --release\n./target/release/shellune",
            )
            .expect("the record is created");
        let snippet = book.get(id).expect("the record is in the library");
        assert_eq!(snippet.name(), "deploy", "the name is trimmed");
        assert_eq!(
            snippet.body(),
            "cargo build --release\n./target/release/shellune",
            "the body is kept exactly as it was entered"
        );
        assert_eq!(snippet.line_count(), 2);
        assert!(snippet.is_multiline());
        assert_eq!(book.len(), 1);
        assert!(!book.is_empty());
    }

    #[test]
    fn an_empty_name_or_body_is_refused_with_its_own_reason() {
        let mut book = SnippetBook::new();
        assert_eq!(book.create("   ", "ls"), Err(SnippetProblem::EmptyName));
        assert_eq!(book.create("ls", "  \n "), Err(SnippetProblem::EmptyBody));
        assert!(book.is_empty(), "a refused record is not stored");
        assert_eq!(
            SnippetProblem::EmptyName.to_string(),
            "a shortcut command needs a name"
        );
        assert!(
            SnippetProblem::EmptyBody
                .to_string()
                .contains("command body"),
            "the body's reason says what is missing"
        );
    }

    #[test]
    fn a_name_or_body_past_its_limit_is_refused_rather_than_shortened() {
        let mut book = SnippetBook::new();
        let long_name = "n".repeat(MAX_SNIPPET_NAME_CHARS + 1);
        assert_eq!(
            book.create(long_name, "ls"),
            Err(SnippetProblem::NameTooLong {
                max: MAX_SNIPPET_NAME_CHARS
            })
        );
        let long_body = "x".repeat(MAX_SNIPPET_BODY_CHARS + 1);
        assert_eq!(
            book.create("big", long_body),
            Err(SnippetProblem::BodyTooLong {
                max: MAX_SNIPPET_BODY_CHARS
            })
        );
        assert!(book.is_empty());
        // Chinese characters count as characters, not bytes.
        let chinese = "命".repeat(MAX_SNIPPET_NAME_CHARS);
        assert!(book.create(chinese, "echo 你好").is_ok());
    }

    #[test]
    fn editing_keeps_the_id_and_deleting_returns_the_record() {
        let mut book = SnippetBook::new();
        let first = book.create("one", "echo 1").expect("created");
        let second = book.create("two", "echo 2").expect("created");

        book.replace(first, "one renamed", "echo 1\n")
            .expect("the record is changed");
        assert_eq!(book.get(first).map(Snippet::name), Some("one renamed"));
        assert_eq!(book.get(second).map(Snippet::name), Some("two"));
        assert_eq!(book.snippets()[0].id(), first, "the order is kept");

        let missing = SnippetId::new();
        assert_eq!(
            book.replace(missing, "gone", "echo"),
            Err(SnippetProblem::Unknown { id: missing })
        );
        assert_eq!(book.len(), 2, "a refused edit changes nothing");

        let removed = book.remove(first).expect("the record is removed");
        assert_eq!(removed.name(), "one renamed");
        assert!(book.get(first).is_none());
        assert!(book.remove(first).is_none(), "removing twice does nothing");
    }

    #[test]
    fn two_records_may_share_a_name_and_are_told_apart_by_id() {
        let mut book = SnippetBook::new();
        let first = book.create("deploy", "make deploy").expect("created");
        let second = book.create("deploy", "make deploy-prod").expect("created");
        assert_ne!(first, second);
        assert_eq!(book.get(first).map(Snippet::body), Some("make deploy"));
        assert_eq!(
            book.get(second).map(Snippet::body),
            Some("make deploy-prod")
        );
    }

    #[test]
    fn a_saved_library_round_trips_through_json() {
        let mut book = SnippetBook::new();
        book.create("deploy", "make deploy\nobsctl status")
            .expect("created");
        let text = serde_json::to_string_pretty(&book).expect("the library encodes");
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(!text.to_lowercase().contains("password"), "{text}");
        assert!(!text.to_lowercase().contains("secret"), "{text}");
        let read: SnippetBook = serde_json::from_str(&text).expect("the library decodes");
        assert_eq!(read, book);
        assert_eq!(read.validate(), Ok(()));
    }

    #[test]
    fn a_library_with_a_record_this_build_would_refuse_is_reported() {
        let empty_name = concat!(
            r#"{"version":1,"snippets":[{"id":"#,
            r#""6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"  ","body":"ls"}]}"#
        );
        let book: SnippetBook = serde_json::from_str(empty_name).expect("the document decodes");
        assert!(
            book.validate().is_err(),
            "a name the editor would refuse is refused on load too"
        );

        let repeated = concat!(
            r#"{"version":1,"snippets":["#,
            r#"{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"a","body":"ls"},"#,
            r#"{"id":"6f1e6f4e-2a2b-4f4c-9e79-6c3a5b4f4d1e","name":"b","body":"pwd"}]}"#
        );
        let book: SnippetBook = serde_json::from_str(repeated).expect("the document decodes");
        assert!(book.validate().is_err());
    }

    #[test]
    fn a_version_this_build_does_not_write_is_reported_with_that_reason() {
        let future = r#"{"version":9,"snippets":[]}"#;
        let book: SnippetBook = serde_json::from_str(future).expect("the document decodes");
        let reason = book
            .validate()
            .expect_err("the version is not this build's");
        assert!(reason.contains("format version 9"), "{reason}");
    }
}
