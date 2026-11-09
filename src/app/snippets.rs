//! Saved commands in the window: the library, the draft and the questions (R6).
//!
//! This module owns what a shortcut command is *in the shell*: the library read from its
//! file, the change that is waiting to be saved, and the record whose deletion is waiting
//! for an answer. It draws nothing and starts nothing; [`crate::app::view`] draws it, and
//! `TerminalPane` is the only place a body is ever written.
//!
//! Two rules are enforced here rather than in the drawing code:
//!
//! * a change reaches the file first and the screen second. A save that fails leaves the
//!   library exactly as the file has it, and reports why, so a list can never show a
//!   command that is not on disk;
//! * a file that could not be read is never written over. Every save is refused while the
//!   library is in that state, because an unreadable library is somebody's data, not an
//!   empty one.

use gpui::Entity;

use shellune_core::{Snippet, SnippetBook, SnippetId, SnippetProblem};
use shellune_storage::SnippetStore;

use crate::app::copy::{self, Locale};
use crate::app::text_field::TextField;

/// A shortcut command's body as it is offered to a terminal.
///
/// The body's own trailing line breaks are dropped: a body saved with a final newline
/// would otherwise submit itself the moment it was inserted. Whether the remaining text
/// needs a confirmation is the terminal's decision, not this module's.
#[must_use]
pub fn body_for_terminal(body: &str) -> &str {
    body.trim_end_matches(['\r', '\n'])
}

/// Why a change to the library did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetSaveError {
    /// The name or the body cannot be used. Nothing was written.
    Problem(SnippetProblem),
    /// The file could not be read or written. Nothing was changed.
    Write(String),
}

impl SnippetSaveError {
    /// The reason in the interface language.
    ///
    /// A refused name or body is worded here, because it is something the user can fix;
    /// a storage failure is shown in the words the storage layer used, like every other
    /// file failure in the window.
    #[must_use]
    pub fn text(&self, locale: Locale) -> String {
        match self {
            Self::Problem(problem) => copy::snippet_problem_text(problem, locale),
            Self::Write(reason) => {
                let ux = copy::ux_for(locale);
                copy::fill(ux.snippet_save_failed_detail, &[("reason", reason)])
            }
        }
    }
}

/// The saved commands, as the window has them.
#[derive(Clone, Debug)]
pub struct SnippetLibrary {
    store: SnippetStore,
    book: SnippetBook,
    /// Why the file could not be read. While this is set, nothing is saved over it.
    read_problem: Option<String>,
    /// Why the last write failed, for the line under the list.
    write_problem: Option<String>,
}

impl SnippetLibrary {
    /// Reads the library beside the other files of the window.
    ///
    /// A damaged or newer file is reported and kept: the list starts empty, every save is
    /// refused, and the dialog names the file that needs repairing. Reading a library
    /// inserts nothing into a terminal and opens no session.
    #[must_use]
    pub fn load(store: SnippetStore) -> Self {
        match store.load() {
            Ok(book) => Self {
                store,
                book,
                read_problem: None,
                write_problem: None,
            },
            Err(error) => Self {
                read_problem: Some(error.to_string()),
                store,
                book: SnippetBook::new(),
                write_problem: None,
            },
        }
    }

    /// Where the commands are saved.
    #[must_use]
    pub fn store(&self) -> &SnippetStore {
        &self.store
    }

    /// Every saved command, in the order the list shows them.
    #[must_use]
    pub fn snippets(&self) -> &[Snippet] {
        self.book.snippets()
    }

    /// One command by id.
    #[must_use]
    pub fn get(&self, id: SnippetId) -> Option<&Snippet> {
        self.book.get(id)
    }

    /// Whether the file can be written at all.
    ///
    /// False only while the file that is there could not be read: writing an empty library
    /// over it would destroy whatever the user really had.
    #[must_use]
    pub fn can_write(&self) -> bool {
        self.read_problem.is_none()
    }

    /// Why the saved file could not be read.
    #[must_use]
    pub fn read_problem(&self) -> Option<&str> {
        self.read_problem.as_deref()
    }

    /// Why the last write failed.
    #[must_use]
    pub fn write_problem(&self) -> Option<&str> {
        self.write_problem.as_deref()
    }

    /// Adds a command and returns its id.
    ///
    /// # Errors
    ///
    /// Returns [`SnippetSaveError`] when the name or body is unusable, or when the file
    /// could not be written. Nothing changes in either case.
    pub fn create(&mut self, name: &str, body: &str) -> Result<SnippetId, SnippetSaveError> {
        let mut next = self.book.clone();
        let id = next.create(name, body).map_err(SnippetSaveError::Problem)?;
        self.commit(next)?;
        Ok(id)
    }

    /// Changes one command, keeping its id and its place in the list.
    ///
    /// # Errors
    ///
    /// Returns [`SnippetSaveError`] when the record is gone, the new name or body is
    /// unusable, or the file could not be written. Nothing changes in those cases.
    pub fn replace(
        &mut self,
        id: SnippetId,
        name: &str,
        body: &str,
    ) -> Result<(), SnippetSaveError> {
        let mut next = self.book.clone();
        next.replace(id, name, body)
            .map_err(SnippetSaveError::Problem)?;
        self.commit(next)
    }

    /// Removes one command. Returns whether there was one to remove.
    ///
    /// # Errors
    ///
    /// Returns [`SnippetSaveError`] when the file could not be written; the list then
    /// still holds the command, because the file still does.
    pub fn remove(&mut self, id: SnippetId) -> Result<bool, SnippetSaveError> {
        let mut next = self.book.clone();
        let removed = next.remove(id).is_some();
        if removed {
            self.commit(next)?;
        }
        Ok(removed)
    }

    /// Writes `next`, and adopts it only once the file has it.
    fn commit(&mut self, next: SnippetBook) -> Result<(), SnippetSaveError> {
        if let Some(problem) = self.read_problem.clone() {
            let reason = format!("{} · {}", self.store.path().display(), problem.trim());
            self.write_problem = Some(reason.clone());
            return Err(SnippetSaveError::Write(reason));
        }
        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                self.write_problem = None;
                Ok(())
            }
            Err(error) => {
                // The storage layer's own words: a path, a permission, a full disk. The
                // list keeps what it had, because the file still holds it.
                let reason = error.to_string();
                self.write_problem = Some(reason.clone());
                Err(SnippetSaveError::Write(reason))
            }
        }
    }
}

/// The editor that is open over the list, and what it is editing.
pub struct SnippetEditor {
    /// The command being changed; `None` while a new one is being written.
    pub id: Option<SnippetId>,
    /// The name field.
    pub name: Entity<TextField>,
    /// The body field. It keeps line breaks, drawn as `\n`.
    pub body: Entity<TextField>,
    /// Why the last save was refused, in the interface language.
    pub problem: Option<String>,
}

impl SnippetEditor {
    /// Whether this editor writes a new command rather than changing one.
    #[must_use]
    pub fn is_new(&self) -> bool {
        self.id.is_none()
    }
}

/// The manager's own state: the editor, the deletion question and the row the keyboard is
/// on. The library itself is [`SnippetLibrary`]; nothing here is saved.
#[derive(Default)]
pub struct SnippetsPanel {
    editor: Option<SnippetEditor>,
    pending_delete: Option<SnippetId>,
    selected: usize,
}

impl SnippetsPanel {
    /// An empty panel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            editor: None,
            pending_delete: None,
            selected: 0,
        }
    }

    /// The row the keyboard is on.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Moves the row highlight inside `len` rows.
    pub fn select(&mut self, index: usize, len: usize) {
        self.selected = if len == 0 { 0 } else { index.min(len - 1) };
    }

    /// Steps the highlighted row, wrapping at both ends.
    pub fn step(&mut self, delta: isize, len: usize) {
        self.selected = crate::app::palette::step(self.selected, delta, len);
    }

    /// The command whose deletion is waiting for an answer.
    #[must_use]
    pub fn pending_delete(&self) -> Option<SnippetId> {
        self.pending_delete
    }

    /// Asks whether one command may be deleted.
    pub fn ask_delete(&mut self, id: SnippetId) {
        self.pending_delete = Some(id);
    }

    /// Drops the deletion question without deleting anything.
    pub fn keep(&mut self) {
        self.pending_delete = None;
    }

    /// The editor, while one is open.
    #[must_use]
    pub fn editor(&self) -> Option<&SnippetEditor> {
        self.editor.as_ref()
    }

    /// The editor, when the drawing code has to read what its fields hold.
    #[must_use]
    pub fn editor_mut(&mut self) -> Option<&mut SnippetEditor> {
        self.editor.as_mut()
    }

    /// Opens an editor, which answers any deletion question that was standing.
    pub fn begin_edit(&mut self, editor: SnippetEditor) {
        self.editor = Some(editor);
        self.pending_delete = None;
    }

    /// Closes the editor, keeping the row highlight where it was.
    pub fn end_edit(&mut self) -> Option<SnippetEditor> {
        self.editor.take()
    }

    /// Whether an editor is open.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }
}

/// The command body as one line, for a list row: breaks become a visible mark.
///
/// A row has one line's height, so the body is shown on one line; the break it contains is
/// still visible there rather than looking like a space that was typed.
#[must_use]
pub fn body_preview(body: &str, max_chars: usize) -> String {
    let flat = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut preview = String::new();
    let mut used = 0;
    for character in flat.chars() {
        let piece = if character == '\n' { "\\n" } else { "" };
        let added = if character == '\n' {
            2
        } else {
            character.len_utf8()
        };
        if used + added > max_chars {
            preview.push('…');
            return preview;
        }
        used += added;
        if piece.is_empty() {
            preview.push(character);
        } else {
            preview.push_str(piece);
        }
    }
    preview
}

/// Checks a command as the editor would, without saving it.
///
/// The dialog asks this before it builds a picture of what will be saved, so the same rule
/// that refuses a save is the one that draws the warning.
#[must_use]
pub fn draft_problem(name: &str, body: &str) -> Option<SnippetProblem> {
    shellune_core::check_snippet_name(name)
        .and_then(|()| shellune_core::check_snippet_body(body))
        .err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "shellune-app-snippets-{}-{}-{tag}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_library_reads_its_file_and_saves_through_it() {
        let dir = scratch("round-trip");
        let mut library = SnippetLibrary::load(SnippetStore::at(&dir));
        assert!(library.can_write());
        let id = library
            .create("deploy", "make deploy")
            .expect("the command is saved");
        assert_eq!(library.snippets().len(), 1);
        assert_eq!(library.get(id).map(Snippet::name), Some("deploy"));

        // The restart: a new library on the same directory.
        let restarted = SnippetLibrary::load(SnippetStore::at(&dir));
        assert_eq!(restarted.snippets().len(), 1);
        assert_eq!(restarted.snippets()[0].id(), id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refused_draft_changes_neither_the_list_nor_the_file() {
        let dir = scratch("refused");
        let mut library = SnippetLibrary::load(SnippetStore::at(&dir));
        let error = library
            .create("  ", "ls")
            .expect_err("an empty name is refused");
        assert_eq!(error, SnippetSaveError::Problem(SnippetProblem::EmptyName));
        assert!(library.snippets().is_empty());
        assert!(
            !SnippetStore::at(&dir).path().exists(),
            "a refused draft writes no file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_library_is_never_written_over_and_closes_every_save() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).expect("dir");
        let store = SnippetStore::at(&dir);
        std::fs::write(store.path(), b"{not json").expect("write");
        let mut library = SnippetLibrary::load(store.clone());
        assert!(!library.can_write());
        assert!(library.read_problem().is_some());
        assert!(library.snippets().is_empty());

        let error = library
            .create("deploy", "make deploy")
            .expect_err("nothing may be written over an unreadable file");
        assert!(matches!(error, SnippetSaveError::Write(_)));
        assert_eq!(
            std::fs::read(store.path()).expect("the file is still there"),
            b"{not json".to_vec(),
            "the unreadable file is exactly as it was"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removing_a_command_removes_it_from_the_file_too() {
        let dir = scratch("remove");
        let mut library = SnippetLibrary::load(SnippetStore::at(&dir));
        let id = library.create("one", "echo one").expect("saved");
        assert!(library.remove(id).expect("removed"));
        assert!(library.snippets().is_empty());
        assert!(!library.remove(id).expect("nothing to remove"));
        let restarted = SnippetLibrary::load(SnippetStore::at(&dir));
        assert!(restarted.snippets().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_inserted_body_drops_its_trailing_line_breaks() {
        assert_eq!(body_for_terminal("make deploy\n"), "make deploy");
        assert_eq!(body_for_terminal("one\ntwo\r\n"), "one\ntwo");
        assert_eq!(
            body_for_terminal("one\n\ntwo"),
            "one\n\ntwo",
            "a break inside the body is part of the command"
        );
    }

    #[test]
    fn a_preview_shows_line_breaks_and_stops_at_its_limit() {
        assert_eq!(
            body_preview("echo one\necho two", 40),
            "echo one\\necho two"
        );
        assert_eq!(body_preview("echo one", 7), "echo on…");
        assert_eq!(body_preview("你好", 40), "你好");
    }

    #[test]
    fn the_panel_keeps_its_row_inside_the_list() {
        let mut panel = SnippetsPanel::new();
        assert_eq!(panel.selected(), 0);
        panel.step(1, 3);
        assert_eq!(panel.selected(), 1);
        panel.step(-2, 3);
        assert_eq!(panel.selected(), 2, "stepping wraps");
        panel.select(9, 3);
        assert_eq!(panel.selected(), 2, "a stale row is clamped");
        panel.select(9, 0);
        assert_eq!(panel.selected(), 0);
    }

    #[test]
    fn a_deletion_question_is_answered_one_way_or_the_other() {
        let mut panel = SnippetsPanel::new();
        let id = SnippetId::new();
        panel.ask_delete(id);
        assert_eq!(panel.pending_delete(), Some(id));
        panel.keep();
        assert_eq!(panel.pending_delete(), None);
        panel.ask_delete(id);
        assert!(panel.editor().is_none());
        panel.end_edit();
        assert!(!panel.is_editing());
    }

    #[test]
    fn a_draft_is_checked_with_the_rule_the_library_uses() {
        assert_eq!(draft_problem(" ", "ls"), Some(SnippetProblem::EmptyName));
        assert_eq!(draft_problem("ls", "ls"), None);
    }
}
