//! The Shellune shell view: navigation rail, header, destination panel, status bar.
//!
//! The composition follows the navigation model in [`crate::app::nav`] and the
//! state model in [`crate::app::workspace`]. Render functions never decide whether a
//! capability exists: they read [`SectionState`] and draw what it reports, which is
//! what keeps a planned or failed section from looking finished.
//!
//! Layout, in one sentence: a fixed navigation rail on the left, a header naming
//! the current object, a scrollable destination panel, and a status bar that
//! carries the product tagline, the counts that are actually non-zero, and the
//! text-size control.

use gpui::prelude::*;
use gpui::{
    Context, Div, Entity, FocusHandle, FontWeight, IntoElement, Render, SharedString, Stateful,
    Window, div, px,
};
use shellune_core::{HostConfig, HostId, LocalShell, Session, SessionId, SessionState, Transfer};
use shellune_storage::{HostBook, HostStore, ImportOutcome, ImportPreview, ImportedHost};
use shellune_terminal::GridSize;

use crate::app::actions::{
    CycleTextSize, DismissNotice, GoToHosts, GoToLocalShells, GoToSessions, GoToTransfers,
    NextDestination, PreviousDestination, ToggleLanguage,
};
use crate::app::copy::{self, Locale, Strings};
use crate::app::hosts::{
    HostDraft, HostField, HostView, HostsPanel, ImportPanel, format_unix, group_row_offsets,
    import_message,
};
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::terminal::{PaneState, TerminalPane};
use crate::app::text_field::TextField;
use crate::app::theme::{self, Metrics, Tone, palette, spacing};
use crate::app::workspace::{
    Failure, PrimaryOutcome, Recovery, SectionState, SectionStatus, Workspace,
};
use crate::app::{components, theme::radius};

/// The application shell view.
pub struct ShelluneView {
    workspace: Workspace,
    root_focus: FocusHandle,
    destination_focus: Vec<FocusHandle>,
    /// The one local terminal this step delivers; multi-session arrives later.
    pane: Option<Entity<TerminalPane>>,
    pane_session: Option<SessionId>,
    shell_path: Entity<TextField>,
    custom_shell_open: bool,
    /// Where the saved hosts live.
    store: HostStore,
    /// What that file really held the last time it was read or written.
    book: HostBook,
    /// The host list views, the open form and the last message.
    hosts: HostsPanel,
    hosts_search: Entity<TextField>,
    /// The five form fields, present only while the editor is open.
    host_fields: Option<HostFields>,
    /// The SSH configuration import panel and what the last read of a file showed.
    import: ImportPanel,
    /// The configuration path field, present only while that panel is open.
    import_path: Option<Entity<TextField>>,
}

/// The five fields of the host editor, in form order.
struct HostFields {
    name: Entity<TextField>,
    address: Entity<TextField>,
    port: Entity<TextField>,
    user: Entity<TextField>,
    group: Entity<TextField>,
}

impl HostFields {
    fn get(&self, field: HostField) -> &Entity<TextField> {
        match field {
            HostField::Name => &self.name,
            HostField::Address => &self.address,
            HostField::Port => &self.port,
            HostField::User => &self.user,
            HostField::Group => &self.group,
        }
    }
}

impl ShelluneView {
    /// Builds the shell in its initial state, reading the saved hosts.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_workspace(cx, Workspace::new())
    }

    /// Builds the shell around an existing workspace, used by tests and by later
    /// tasks that restore state before the first frame.
    pub fn with_workspace(cx: &mut Context<Self>, workspace: Workspace) -> Self {
        Self::with_store(cx, workspace, default_store())
    }

    /// Builds the shell around an existing workspace and host store.
    ///
    /// The hosts section the caller passed decides where the records come from: a
    /// planned section means "read the store", anything else means the caller has
    /// already established the state and the store is only where saves will go.
    pub fn with_store(cx: &mut Context<Self>, workspace: Workspace, store: HostStore) -> Self {
        let placeholder = workspace.strings().shell_custom_placeholder;
        let search_placeholder = workspace.strings().hosts_search_placeholder;
        let shell_path = cx.new(|cx| TextField::new(placeholder, cx));
        let hosts_search = cx.new(|cx| TextField::new(search_placeholder, cx));
        let mut view = Self {
            workspace,
            root_focus: cx.focus_handle(),
            destination_focus: Destination::ALL.iter().map(|_| cx.focus_handle()).collect(),
            pane: None,
            pane_session: None,
            shell_path,
            custom_shell_open: false,
            store,
            book: HostBook::new(),
            hosts: HostsPanel::new(),
            hosts_search,
            host_fields: None,
            import: ImportPanel::new(),
            import_path: None,
        };
        view.bind_shell_path(cx);
        view.bind_host_search(cx);
        view.restore_hosts(cx);
        view
    }

    /// Reads the saved hosts, or adopts the section the caller already built.
    fn restore_hosts(&mut self, cx: &mut Context<Self>) {
        if matches!(self.workspace.hosts().status(), SectionStatus::Planned) {
            self.reload_hosts(cx);
            return;
        }
        // A restored section is a real result from the caller; seeding the book with
        // it keeps the list and the records from disagreeing. Records that break the
        // invariants are dropped rather than rendered: the store is the only place
        // that can say what is really saved.
        self.book =
            HostBook::from_hosts(self.workspace.hosts().items().to_vec()).unwrap_or_default();
    }

    /// The saved records this shell last read or wrote.
    ///
    /// Read by the shell tests, and by the steps that record a real connection.
    #[allow(dead_code)]
    #[must_use]
    pub fn book(&self) -> &HostBook {
        &self.book
    }

    /// The host list views, the open form and the last message.
    #[allow(dead_code)]
    #[must_use]
    pub fn hosts_state(&self) -> &HostsPanel {
        &self.hosts
    }

    /// The import panel and what the last read of a configuration file showed.
    #[cfg(test)]
    pub fn import_state(&self) -> &ImportPanel {
        &self.import
    }

    /// Selects one host list view before the first frame, so a test can observe what
    /// that view draws on its own instead of what an earlier frame left behind.
    #[cfg(test)]
    pub fn select_host_view_for_test(&mut self, view: HostView, cx: &mut Context<Self>) {
        self.hosts.select_view(view);
        cx.notify();
    }

    /// The terminal currently shown, if one was started.
    #[cfg(test)]
    pub fn pane(&self) -> Option<&Entity<TerminalPane>> {
        self.pane.as_ref()
    }

    /// Whether the free-text shell path field is on screen.
    #[cfg(test)]
    pub fn custom_shell_open(&self) -> bool {
        self.custom_shell_open
    }

    /// What the free-text shell path field holds.
    #[cfg(test)]
    pub fn shell_path_text(&self, cx: &gpui::App) -> String {
        self.shell_path.read(cx).text().to_owned()
    }

    /// The current workspace state.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Focus handle of the shell root, which owns the key context.
    pub fn root_focus(&self) -> &FocusHandle {
        &self.root_focus
    }

    /// Focus handle of one navigation entry.
    pub fn destination_focus(&self, destination: Destination) -> &FocusHandle {
        &self.destination_focus[destination.index()]
    }

    fn strings(&self) -> &'static Strings {
        self.workspace.strings()
    }

    fn metrics(&self) -> Metrics {
        self.workspace.metrics()
    }

    fn primary_label(&self, destination: Destination, strings: &Strings) -> &'static str {
        match destination {
            Destination::LocalShells => strings.primary_local,
            Destination::Hosts => strings.primary_hosts,
            Destination::Sessions => strings.primary_sessions,
            Destination::Transfers => strings.primary_transfers,
        }
    }

    /// Selects a destination, moving keyboard focus with it.
    fn select(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.select(destination);
        let handle = self.destination_focus(destination).clone();
        window.focus(&handle);
        cx.notify();
    }

    fn request_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.destination() == Destination::LocalShells {
            self.start_from_picker(window, cx);
            return;
        }
        if self.workspace.destination() == Destination::Hosts {
            // A destination whose records could not be read offers reading them
            // again; every other state of it offers a new host.
            if self.workspace.state(Destination::Hosts) == SectionState::Failed {
                self.reload_hosts(cx);
            } else {
                self.open_create_host(cx);
            }
            return;
        }
        if self.workspace.request_primary() == PrimaryOutcome::Unavailable {
            // Keep focus inside the shell so keyboard users are not dropped.
            let handle = self.root_focus.clone();
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Connects the free-text shell field to this view, so Enter starts a shell.
    fn bind_shell_path(&mut self, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        self.shell_path.update(cx, |field, _cx| {
            field.on_submit(move |text, window, app| {
                view.update(app, |view, cx| view.start_custom_shell(text, window, cx))
                    .unwrap_or(false)
            });
        });
    }

    // -----------------------------------------------------------------------
    // Host management (R3)
    // -----------------------------------------------------------------------

    /// Mirrors the search field into the panel, so the list filters as the user types.
    fn bind_host_search(&mut self, cx: &mut Context<Self>) {
        let search = self.hosts_search.clone();
        cx.observe(&search, |view, field, cx| {
            let phrase = field.read(cx).text().to_owned();
            if view.hosts.search() != phrase {
                view.hosts.set_search(phrase);
                cx.notify();
            }
        })
        .detach();
    }

    /// Mirrors one form field into the draft and lets Enter save the form.
    fn bind_host_field(&mut self, field: HostField, cx: &mut Context<Self>) {
        let Some(entity) = self
            .host_fields
            .as_ref()
            .map(|fields| fields.get(field).clone())
        else {
            return;
        };

        let view = cx.entity().downgrade();
        entity.update(cx, |text_field, _cx| {
            text_field.on_submit(move |_text, _window, app| {
                view.update(app, |view, cx| {
                    view.save_host(cx);
                    // The field keeps its text: when the save was refused, the reason
                    // points at what the user typed and must still be there.
                    false
                })
                .unwrap_or(false)
            });
        });

        cx.observe(&entity, move |view, text_field, cx| {
            let text = text_field.read(cx).text().to_owned();
            if let Some(draft) = view.hosts.draft_mut() {
                draft.set_text(field, text);
            }
            cx.notify();
        })
        .detach();
    }

    /// Reads the host file again and reports what it really holds.
    fn reload_hosts(&mut self, cx: &mut Context<Self>) {
        match self.store.load() {
            Ok(book) => {
                self.book = book;
                self.hosts.clear_message();
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
            }
            Err(error) => {
                // A file that cannot be read is not an empty list: the section keeps
                // the reason the storage layer produced and holds no records, so no
                // host is shown that the file does not really contain.
                self.book = HostBook::new();
                self.host_fields = None;
                self.workspace
                    .fail_hosts(Failure::new(error.to_string(), Recovery::Retry));
            }
        }
        cx.notify();
    }

    /// Opens an empty form for a new host.
    fn open_create_host(&mut self, cx: &mut Context<Self>) {
        self.hosts.open_create();
        self.open_host_fields(cx);
    }

    /// Opens the form filled with a saved host.
    fn open_edit_host(&mut self, host: &HostConfig, cx: &mut Context<Self>) {
        self.hosts.open_edit(host);
        self.open_host_fields(cx);
    }

    fn open_host_fields(&mut self, cx: &mut Context<Self>) {
        let draft = self
            .hosts
            .draft()
            .cloned()
            .unwrap_or_else(HostDraft::create);
        let strings = self.strings();
        let name = cx.new(|cx| {
            TextField::with_text(strings.host_field_name, draft.text(HostField::Name), cx)
        });
        let address = cx.new(|cx| {
            TextField::with_text(
                strings.host_field_address,
                draft.text(HostField::Address),
                cx,
            )
        });
        let port = cx.new(|cx| {
            TextField::with_text(strings.host_field_port, draft.text(HostField::Port), cx)
        });
        let user = cx.new(|cx| {
            TextField::with_text(strings.host_field_user, draft.text(HostField::User), cx)
        });
        let group = cx.new(|cx| {
            TextField::with_text(strings.host_field_group, draft.text(HostField::Group), cx)
        });
        self.host_fields = Some(HostFields {
            name,
            address,
            port,
            user,
            group,
        });
        for field in HostField::ALL {
            self.bind_host_field(field, cx);
        }
        cx.notify();
    }

    /// Closes the form without touching the saved records.
    fn close_host_editor(&mut self, cx: &mut Context<Self>) {
        self.hosts.close_editor();
        self.host_fields = None;
        cx.notify();
    }

    /// Validates the form and stores it when the domain accepts it.
    fn save_host(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.hosts.draft().cloned() else {
            return;
        };
        let saved = draft.editing().and_then(|id| self.book.host(id)).cloned();
        let edit = match draft.build(saved.as_ref()) {
            Ok(edit) => edit,
            Err(reason) => {
                // The form stays open with what the user typed, and the reason names
                // the field the domain refused.
                self.hosts.set_message(reason, true);
                cx.notify();
                return;
            }
        };

        let mut next = self.book.clone();
        if let Err(error) = next.apply(&edit) {
            self.hosts.set_message(error.to_string(), true);
            cx.notify();
            return;
        }
        self.commit_hosts(next, self.strings().hosts_saved, cx);
    }

    /// Saves the records, and only then adopts them as the current state.
    fn commit_hosts(&mut self, next: HostBook, message: &str, cx: &mut Context<Self>) {
        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                self.hosts.close_editor();
                self.host_fields = None;
                self.hosts.set_message(message.to_owned(), false);
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
            }
            // The records on disk did not change, so neither does the list: the
            // message carries the reason the write produced.
            Err(error) => self.hosts.set_message(error.to_string(), true),
        }
        cx.notify();
    }

    /// Asks the deletion question for one host.
    fn ask_delete_host(&mut self, id: HostId, cx: &mut Context<Self>) {
        self.host_fields = None;
        self.hosts.ask_delete(id);
        cx.notify();
    }

    /// Drops the deletion question without touching anything.
    fn cancel_delete_host(&mut self, cx: &mut Context<Self>) {
        self.hosts.cancel_delete();
        cx.notify();
    }

    /// Deletes the host the question is about, once the answer is stored.
    fn confirm_delete_host(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.hosts.take_delete() else {
            return;
        };
        let mut next = self.book.clone();
        if let Err(error) = next.remove(id) {
            self.hosts.set_message(error.to_string(), true);
            cx.notify();
            return;
        }
        self.commit_hosts(next, self.strings().hosts_deleted, cx);
    }

    // -----------------------------------------------------------------------
    // SSH configuration import (R5)
    // -----------------------------------------------------------------------

    /// Mirrors the configuration path field into the panel and lets Enter preview it.
    fn bind_import_path(&mut self, cx: &mut Context<Self>) {
        let Some(field) = self.import_path.clone() else {
            return;
        };

        let view = cx.entity().downgrade();
        field.update(cx, |text_field, _cx| {
            text_field.on_submit(move |_text, _window, app| {
                view.update(app, |view, cx| {
                    view.preview_import(cx);
                    false
                })
                .unwrap_or(false)
            });
        });

        cx.observe(&field, |view, text_field, cx| {
            let text = text_field.read(cx).text().to_owned();
            // A different path is a different question, so the previous preview goes
            // away instead of being applied to a file nobody looked at.
            view.import.set_path(text);
            cx.notify();
        })
        .detach();
    }

    /// Opens the import panel on the file this platform's ssh would read.
    ///
    /// A second click does nothing: the panel already shows a reading of a file, and
    /// replacing it would throw away a preview the user may be about to apply.
    fn open_import(&mut self, cx: &mut Context<Self>) {
        if self.import.is_open() {
            return;
        }
        let path = match shellune_storage::default_config_path() {
            Some(path) => path.display().to_string(),
            None => shellune_storage::DEFAULT_CONFIG_TEXT.to_owned(),
        };
        self.open_import_at(path, cx);
    }

    /// Opens the import panel on a named file, through the same path a click takes.
    #[cfg(test)]
    pub fn open_import_for_test(&mut self, path: &str, cx: &mut Context<Self>) {
        self.open_import_at(path.to_owned(), cx);
    }

    fn open_import_at(&mut self, path: String, cx: &mut Context<Self>) {
        self.import.open(path);
        self.hosts.close_editor();
        self.host_fields = None;
        self.hosts.clear_message();
        let placeholder = self.strings().import_path_placeholder;
        let text = self.import.path().to_owned();
        self.import_path = Some(cx.new(|cx| TextField::with_text(placeholder, text, cx)));
        self.bind_import_path(cx);
        cx.notify();
    }

    /// Closes the import panel without touching the saved hosts.
    fn close_import(&mut self, cx: &mut Context<Self>) {
        self.import.close();
        self.import_path = None;
        cx.notify();
    }

    /// Reads the named file and shows what importing it would do.
    ///
    /// A failure keeps the reason the importer produced, including the line that
    /// could not be parsed, and leaves the saved hosts alone.
    fn preview_import(&mut self, cx: &mut Context<Self>) {
        let path = self.import.path().trim().to_owned();
        if path.is_empty() {
            self.import.set_error(self.strings().import_path_required);
            cx.notify();
            return;
        }
        match ImportPreview::from_file(&path) {
            Ok(preview) => self.import.set_preview(preview),
            Err(error) => self.import.set_error(error.to_string()),
        }
        cx.notify();
    }

    /// Saves the hosts the preview offers, and says what it did not save.
    fn apply_import(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.import.preview().cloned() else {
            return;
        };
        let plan = preview.plan(&self.book);
        let imported = plan.len();
        // A host whose name was already saved is not imported at all, so it must not
        // be counted as one that was imported and needs a look.
        let review = preview
            .hosts()
            .iter()
            .filter(|host| host.outcome() == ImportOutcome::NeedsReview && host.is_importable())
            .filter(|host| {
                !plan
                    .skipped()
                    .iter()
                    .any(|skipped| skipped.alias() == host.alias())
            })
            .count();
        let skipped = plan.skipped().len();

        let mut next = self.book.clone();
        if let Err(error) = plan.apply(&mut next) {
            self.import.set_apply_error(error.to_string());
            cx.notify();
            return;
        }

        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                let items = self.book.hosts().to_vec();
                self.workspace.replace_hosts(SectionStatus::Ready, items);
                self.hosts.set_message(
                    import_message(self.strings(), imported, review, skipped),
                    false,
                );
                self.import.close();
                self.import_path = None;
            }
            // The file did not change, so the preview stays: the same hosts can be
            // applied again once the write is possible.
            Err(error) => self.import.set_apply_error(error.to_string()),
        }
        cx.notify();
    }

    /// Records that a host was really reached, and stores the record.
    ///
    /// The remote-connection step calls this after a session reached
    /// `Connected`. Nothing in host management itself calls it, which is why saving
    /// a host never makes it appear in the recent view.
    ///
    /// Returns whether the record reached the file.
    #[allow(dead_code)]
    pub fn record_host_connection(
        &mut self,
        id: HostId,
        at_unix: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut next = self.book.clone();
        if let Err(error) = next.record_connection(id, at_unix) {
            self.hosts.set_message(error.to_string(), true);
            cx.notify();
            return false;
        }
        match self.store.save(&next) {
            Ok(()) => {
                self.book = next;
                cx.notify();
                true
            }
            Err(error) => {
                self.hosts.set_message(error.to_string(), true);
                cx.notify();
                false
            }
        }
    }

    /// Starts the shell named in the free-text field.
    ///
    /// The text is taken as written: a wrong path becomes a real launch failure,
    /// which is the only honest answer to a program that does not exist.
    fn start_custom_shell(
        &mut self,
        program: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let program = program.trim().to_owned();
        if program.is_empty() {
            return false;
        }
        let shell = LocalShell::new(program.clone(), program);
        self.start_shell(shell, Some(window), cx);
        if self.pane.is_some() {
            // The shell that started owns the keyboard, so what the user types next
            // goes to it. The field closes and, because this returns true, empties:
            // a second Return over the same path would start the shell again
            // instead of reaching it.
            self.custom_shell_open = false;
            cx.notify();
        }
        self.pane.is_some()
    }

    /// Starts whichever shell the picker currently points at.
    fn start_from_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = if self.custom_shell_open {
            self.shell_path.read(cx).text().trim().to_owned()
        } else {
            String::new()
        };
        let shell = if typed.is_empty() {
            match self.workspace.selected_shell() {
                Some(shell) => shell.clone(),
                None => {
                    let reason = self.strings().shell_none_found.to_owned();
                    self.workspace
                        .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
                    cx.notify();
                    return;
                }
            }
        } else {
            LocalShell::new(typed.clone(), typed)
        };
        self.start_shell(shell, Some(window), cx);
    }

    /// Starts a shell and, when it really started, records the session.
    ///
    /// A launch failure replaces the section with the reason and keeps no pane, so
    /// the destination cannot show a terminal that has no process behind it.
    fn start_shell(
        &mut self,
        shell: LocalShell,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.pane_session {
            self.workspace.session_closed(session);
        }
        self.pane = None;
        self.pane_session = None;

        let grid = GridSize::initial();
        let font_size = self.workspace.local_font_size();
        let locale = self.workspace.locale();
        let pane =
            cx.new(|pane_cx| TerminalPane::start(shell.clone(), grid, font_size, locale, pane_cx));
        let state = pane.read(cx).state().clone();
        if !state.accepts_input() {
            let reason = state
                .failure()
                .map_or_else(|| state.label(locale), |failure| failure.to_string());
            self.workspace
                .fail_local_start(Failure::new(reason, Recovery::ChooseShell));
            cx.notify();
            return;
        }

        let session = self.workspace.register_local_session(&shell);
        let id = session.id();
        self.workspace.session_connected(id);
        // The size the pane actually uses is the one the next session starts with.
        self.workspace
            .set_local_font_size(pane.read(cx).font_size());
        cx.observe(&pane, |view, pane, cx| view.sync_pane(pane, cx))
            .detach();
        if let Some(window) = window {
            let handle = pane.read(cx).focus_handle().clone();
            window.focus(&handle);
        }
        self.pane = Some(pane);
        self.pane_session = Some(id);
        cx.notify();
    }

    /// Mirrors what the pane reports into the session list.
    ///
    /// The pane owns the truth about the process; the list only repeats it, so a
    /// shell that exited cannot stay listed as running.
    fn sync_pane(&mut self, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
        let Some(session) = self.pane_session else {
            return;
        };
        let state = pane.read(cx).state().clone();
        match state {
            PaneState::Live => {
                self.workspace.session_connected(session);
            }
            PaneState::Exited { report } => {
                // A shell that exits on its own is still over. Reporting the status it
                // exited with is the difference between "the session ended" and a
                // session list that keeps claiming a process that is gone.
                let failure = report
                    .session_failure()
                    .unwrap_or(shellune_core::SessionFailure::ProcessExited { code: Some(0) });
                self.workspace.session_failed(session, failure);
            }
            PaneState::Failed(failure) => {
                self.workspace.session_failed(session, failure);
            }
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Hosts destination (R3)
    // -----------------------------------------------------------------------

    /// The hosts destination: where the records live, the search and view switch,
    /// the form, the deletion question and the list the current view selects.
    ///
    /// A store that cannot be read renders as a state card with its reason and a
    /// retry, because "no hosts" and "the file is broken" are different answers.
    fn hosts_destination(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let state = self.workspace.state(Destination::Hosts);

        match state {
            SectionState::Planned | SectionState::Loading => {
                return self.section_card(Destination::Hosts, state);
            }
            SectionState::Failed => {
                return self
                    .section_card(Destination::Hosts, state)
                    .child(self.text_button(
                        SharedString::from("hosts.retry"),
                        strings.hosts_retry,
                        Tone::Accent,
                        cx,
                        |view, cx| view.reload_hosts(cx),
                    ));
            }
            SectionState::Empty | SectionState::Populated => {}
        }

        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(metrics.scale(spacing::SNUG))
            .child(self.hosts_storage_line());

        if let Some(message) = self.hosts.message() {
            column = column.child(
                div()
                    .debug_selector(|| "hosts.message".to_owned())
                    .text_size(metrics.label_size())
                    .text_color(if message.failed() {
                        Tone::Danger.text()
                    } else {
                        Tone::Positive.text()
                    })
                    .child(SharedString::from(message.text().to_owned())),
            );
        }

        // An empty store with no search, no open form and no open import explains
        // itself; anything else shows the tools and the list.
        if self.book.hosts().is_empty()
            && self.hosts.search().trim().is_empty()
            && !self.hosts.is_editing()
            && !self.import.is_open()
        {
            return column
                .child(self.section_card(Destination::Hosts, state))
                .child(self.hosts_import_entry(cx));
        }

        column = column.child(self.hosts_toolbar(cx));
        if self.import.is_open() {
            column = column.child(self.import_panel(cx));
        }
        if let Some(fields) = &self.host_fields {
            column = column.child(self.host_editor(fields, cx));
        }
        if let Some(id) = self.hosts.pending_delete() {
            column = column.child(self.host_delete_bar(id, cx));
        }
        column.child(self.hosts_list(cx))
    }

    /// Where the records are stored, said plainly.
    fn hosts_storage_line(&self) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "hosts.storage".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.hosts_storage_label),
            )
            .child(
                div()
                    .truncate()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(SharedString::from(self.store.path().display().to_string())),
            )
    }

    /// The search field and the three list views.
    fn hosts_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut row = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "hosts.toolbar".to_owned())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .debug_selector(|| "hosts.search".to_owned())
                    .child(self.hosts_search.clone()),
            );

        for view in HostView::ALL {
            let selected = self.hosts.view() == view;
            row = row.child(
                div()
                    .id(view.debug_selector())
                    .debug_selector(move || view.debug_selector().to_owned())
                    .px(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(2.0))
                    .flex_none()
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(if selected {
                        palette::ACCENT_TINT
                    } else {
                        palette::SURFACE_RAISED
                    }))
                    .border_1()
                    .border_color(theme::swatch(if selected {
                        palette::ACCENT
                    } else {
                        palette::BORDER
                    }))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(if selected {
                        palette::ACCENT_ON_TINT
                    } else {
                        palette::TEXT_SECONDARY
                    }))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.hosts.select_view(view);
                        cx.notify();
                    }))
                    .child(view.label(strings)),
            );
        }
        // Importing sits beside the views because it is how a list gets its first
        // records; the panel it opens states its limits before anything is saved.
        row.child(self.import_button(cx))
    }

    /// The button that opens the import panel.
    fn import_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.text_button(
            SharedString::from("hosts.import"),
            self.strings().import_open,
            Tone::Accent,
            cx,
            |view, cx| view.open_import(cx),
        )
    }

    /// The import entry an empty list offers, since a first run has no hosts to show
    /// beside it.
    fn hosts_import_entry(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::SNUG))
            .child(self.import_button(cx))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.import_read_only),
            )
    }

    /// The form that creates or changes one host.
    fn host_editor(&self, fields: &HostFields, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let editing = self.hosts.draft().and_then(HostDraft::editing).is_some();

        let mut card = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .p(metrics.scale(spacing::LOOSE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "hosts.editor".to_owned())
            .child(
                div()
                    .text_size(metrics.title_size())
                    .font_weight(metrics.title_weight())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(if editing {
                        strings.host_editor_edit
                    } else {
                        strings.host_editor_new
                    }),
            );

        for field in HostField::ALL {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .w(metrics.scale(64.0))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(field.label(strings)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .debug_selector(move || field.debug_selector().to_owned())
                            .child(fields.get(field).clone()),
                    ),
            );
        }

        card.child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .mt(metrics.scale(spacing::HAIRLINE))
                .child(self.text_button(
                    SharedString::from("hosts.save"),
                    strings.host_save,
                    Tone::Accent,
                    cx,
                    |view, cx| view.save_host(cx),
                ))
                .child(self.text_button(
                    SharedString::from("hosts.cancel"),
                    strings.host_cancel,
                    Tone::Neutral,
                    cx,
                    |view, cx| view.close_host_editor(cx),
                )),
        )
    }

    /// The import preview: the file it read, its limits and every host it describes.
    ///
    /// Nothing here is decided by the drawing code: the outcome, the notes and the
    /// count of hosts that can be imported all come from the importer, which is what
    /// keeps a partial import from looking like a complete one.
    fn import_panel(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();

        let path_field = match &self.import_path {
            Some(field) => div()
                .flex_1()
                .min_w(px(0.0))
                .debug_selector(|| "hosts.import.path".to_owned())
                .child(field.clone()),
            None => div().flex_1(),
        };

        let mut card = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .p(metrics.scale(spacing::LOOSE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "hosts.import.panel".to_owned())
            .child(
                div()
                    .text_size(metrics.title_size())
                    .font_weight(metrics.title_weight())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(strings.import_title),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex_none()
                            .w(metrics.scale(64.0))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.import_path_label),
                    )
                    .child(path_field)
                    .child(self.text_button(
                        SharedString::from("hosts.import.preview"),
                        strings.import_preview,
                        Tone::Accent,
                        cx,
                        |view, cx| view.preview_import(cx),
                    )),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.import_read_only),
            );

        if let Some(error) = self.import.error() {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(1.0))
                    .debug_selector(|| "hosts.import.error".to_owned())
                    .child(
                        div()
                            .text_size(metrics.label_size())
                            .text_color(Tone::Danger.text())
                            .child(strings.import_error_title),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(error.to_owned())),
                    ),
            );
            if !self.import.keeps_preview() {
                return card;
            }
        }

        let Some(preview) = self.import.preview() else {
            return card;
        };

        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .debug_selector(|| "hosts.import.source".to_owned())
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(strings.import_source_label),
                )
                .child(
                    div()
                        .truncate()
                        .min_w(px(0.0))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(preview.source().display().to_string())),
                ),
        );

        card = card.child(self.import_limits(preview));

        for (index, host) in preview.hosts().iter().enumerate() {
            card = card.child(self.import_row(index, host));
        }

        let mut actions = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .mt(metrics.scale(spacing::HAIRLINE));

        let offered = preview.importable_count();
        if offered > 0 {
            actions = actions.child(self.text_button(
                SharedString::from("hosts.import.apply"),
                SharedString::from(format!("{} · {offered}", strings.import_apply)),
                Tone::Accent,
                cx,
                |view, cx| view.apply_import(cx),
            ));
        } else {
            actions = actions.child(
                div()
                    .debug_selector(|| "hosts.import.nothing".to_owned())
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.import_nothing),
            );
        }

        card.child(actions.child(self.text_button(
            SharedString::from("hosts.import.close"),
            strings.host_cancel,
            Tone::Neutral,
            cx,
            |view, cx| view.close_import(cx),
        )))
    }

    /// What the file could not contribute, stated before anything is applied.
    fn import_limits(&self, preview: &ImportPreview) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let limits = preview.limits();
        let ready = preview
            .hosts()
            .iter()
            .filter(|host| host.outcome() == ImportOutcome::Ready && host.is_importable())
            .count();

        let mut block = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(1.0))
            .w_full()
            .debug_selector(|| "hosts.import.limits".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.import_limits_title),
            )
            .child(caption_line(
                format!(
                    "{} {} · {} {} · {} {}",
                    strings.import_outcome_ready,
                    ready,
                    strings.import_outcome_review,
                    preview.review_count(),
                    strings.import_outcome_blocked,
                    preview.blocked_count()
                ),
                metrics,
            ));

        if !limits.ignored().is_empty() {
            let keywords: Vec<String> = limits
                .ignored()
                .iter()
                .map(|note| format!("{} (line {})", note.keyword(), note.line()))
                .collect();
            block = block.child(caption_line(
                format!("{} · {}", strings.import_ignored_label, keywords.join(", ")),
                metrics,
            ));
        }
        if !limits.include_lines().is_empty() {
            block = block.child(caption_line(
                format!(
                    "{} · {}",
                    strings.import_include_label,
                    line_list(limits.include_lines())
                ),
                metrics,
            ));
        }
        if !limits.match_lines().is_empty() {
            block = block.child(caption_line(
                format!(
                    "{} · {}",
                    strings.import_match_label,
                    line_list(limits.match_lines())
                ),
                metrics,
            ));
        }
        if !preview.is_partial() {
            block = block.child(caption_line(strings.import_limits_none.to_owned(), metrics));
        }
        block
    }

    /// One host the file describes, with the outcome and every reason behind it.
    fn import_row(&self, index: usize, host: &ImportedHost) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let (tone, label) = match host.outcome() {
            ImportOutcome::Ready => (Tone::Positive, strings.import_outcome_ready),
            ImportOutcome::NeedsReview => (Tone::Warning, strings.import_outcome_review),
            ImportOutcome::Blocked => (Tone::Danger, strings.import_outcome_blocked),
        };
        let chip = if host.is_importable() {
            SharedString::from(label)
        } else {
            SharedString::from(format!("{label} · {}", strings.import_not_offered))
        };

        let mut row = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(1.0))
            .w_full()
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(move || format!("hosts.import.row.{index}"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .min_w(px(0.0))
                            .child(components::status_dot(tone, metrics))
                            .child(components::stacked(
                                host.alias().to_owned(),
                                host.summary(),
                                metrics,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .child(components::chip(chip, tone, metrics)),
                    ),
            );

        for note in host.notes() {
            row = row.child(caption_line(note.reason().to_owned(), metrics));
        }
        row
    }

    /// The question shown before a host is deleted.
    fn host_delete_bar(&self, id: HostId, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let target = self.book.host(id).map_or_else(String::new, |host| {
            format!("{} ({})", host.name(), host.target())
        });

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::SNUG))
            .w_full()
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(Tone::Danger.text())
            .debug_selector(|| "hosts.confirm".to_owned())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(1.0))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(format!(
                                "{} {target}",
                                strings.host_delete_question
                            ))),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.host_delete_note),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.text_button(
                        SharedString::from("hosts.confirm.delete"),
                        strings.host_delete_confirm,
                        Tone::Danger,
                        cx,
                        |view, cx| view.confirm_delete_host(cx),
                    ))
                    .child(self.text_button(
                        SharedString::from("hosts.confirm.cancel"),
                        strings.host_cancel,
                        Tone::Neutral,
                        cx,
                        |view, cx| view.cancel_delete_host(cx),
                    )),
            )
    }

    /// The saved hosts, in the shape the current view selects.
    fn hosts_list(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        // The number of rows the current view lists is part of the selector, so a
        // test can tell "the search narrowed the list" from "the list did not
        // change" without reading text.
        let entries = match self.hosts.view() {
            HostView::All => self.hosts.filtered(&self.book).len(),
            HostView::Groups => self
                .hosts
                .groups(&self.book)
                .iter()
                .map(shellune_storage::Group::len)
                .sum(),
            HostView::Recent => self.hosts.recent(&self.book).len(),
        };
        let mut list = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .debug_selector(move || format!("hosts.list.{entries}"));

        match self.hosts.view() {
            HostView::All => {
                let hosts = self.hosts.filtered(&self.book);
                if hosts.is_empty() {
                    return self.nothing_to_list(list);
                }
                for (index, host) in hosts.into_iter().enumerate() {
                    list = list.child(self.host_row(index, host, cx));
                }
            }
            HostView::Groups => {
                let groups = self.hosts.groups(&self.book);
                if groups.is_empty() {
                    return self.nothing_to_list(list);
                }
                // The row numbering comes from one function that runs across the groups,
                // so two rows of one list can never share an element id.
                let offsets = group_row_offsets(&groups);
                for (group_index, (group, offset)) in groups.iter().zip(offsets).enumerate() {
                    let label = group.label().unwrap_or(strings.hosts_ungrouped);
                    list = list.child(
                        div()
                            .debug_selector(move || format!("hosts.group.{group_index}"))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!("{label} · {}", group.len()))),
                    );
                    for (row_index, host) in (offset..).zip(group.hosts()) {
                        list = list.child(self.host_row(row_index, host, cx));
                    }
                }
            }
            HostView::Recent => {
                let recent = self.hosts.recent(&self.book);
                if recent.is_empty() {
                    return list.child(self.hosts_recent_empty());
                }
                for (index, entry) in recent.into_iter().enumerate() {
                    list = list.child(self.host_recent_row(index, entry, cx));
                }
            }
        }
        list
    }

    /// One saved host with the buttons that change it.
    fn host_row(&self, index: usize, host: &HostConfig, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let id = host.id();
        let edited = host.clone();

        let mut row = components::row(metrics)
            .debug_selector(move || format!("hosts.row.{index}"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(Tone::Neutral, metrics))
                    .child(components::stacked(
                        host.name().to_owned(),
                        host.target(),
                        metrics,
                    )),
            );
        if let Some(group) = host_group_chip(host) {
            row = row.child(components::chip(group, Tone::Neutral, metrics));
        }
        row.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::TIGHT))
                .child(self.text_button(
                    SharedString::from(format!("hosts.edit.{index}")),
                    strings.host_edit,
                    Tone::Accent,
                    cx,
                    move |view, cx| view.open_edit_host(&edited, cx),
                ))
                .child(self.text_button(
                    SharedString::from(format!("hosts.delete.{index}")),
                    strings.host_delete,
                    Tone::Danger,
                    cx,
                    move |view, cx| view.ask_delete_host(id, cx),
                )),
        )
    }

    /// One host that was really reached, with when it happened.
    fn host_recent_row(
        &self,
        index: usize,
        entry: shellune_storage::RecentHost<'_>,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let edited = entry.host.clone();
        let id = entry.host.id();
        let last = format!(
            "{} {} · {} {}",
            strings.host_last_connected,
            format_unix(entry.last_connected_unix),
            entry.connections,
            strings.host_connection_count
        );

        components::row(metrics)
            .debug_selector(move || format!("hosts.recent.{index}"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(Tone::Positive, metrics))
                    .child(components::stacked(
                        entry.host.name().to_owned(),
                        format!("{} · {last}", entry.host.target()),
                        metrics,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(self.text_button(
                        SharedString::from(format!("hosts.recent.edit.{index}")),
                        strings.host_edit,
                        Tone::Accent,
                        cx,
                        move |view, cx| view.open_edit_host(&edited, cx),
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("hosts.recent.delete.{index}")),
                        strings.host_delete,
                        Tone::Danger,
                        cx,
                        move |view, cx| view.ask_delete_host(id, cx),
                    )),
            )
    }

    /// Nothing to list: a search that matched nothing says so, while an empty
    /// book next to an open form just leaves the form to be filled.
    fn nothing_to_list(&self, list: Div) -> Div {
        if self.hosts.search().trim().is_empty() {
            list
        } else {
            list.child(self.hosts_no_match())
        }
    }

    fn hosts_no_match(&self) -> Div {
        let metrics = self.metrics();
        div()
            .debug_selector(|| "hosts.nomatch".to_owned())
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .child(self.strings().hosts_no_match)
    }

    fn hosts_recent_empty(&self) -> Div {
        let metrics = self.metrics();
        div()
            .debug_selector(|| "hosts.recent.empty".to_owned())
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .child(self.strings().hosts_recent_empty)
    }

    /// A small command button that runs one action.
    fn text_button(
        &self,
        id: SharedString,
        label: impl Into<SharedString>,
        tone: Tone,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let selector = id.to_string();
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .flex_none()
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::SMALL))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .bg(theme::swatch(palette::SURFACE))
            .text_size(metrics.label_size())
            .text_color(tone.text())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _event, _window, cx| click(this, cx)))
            .child(label.into())
    }

    fn brand(&self) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::SNUG))
            .py(metrics.scale(spacing::BASE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.display_size())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(product::NAME),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{} · {}",
                                product::PRONUNCIATION,
                                strings.workspace
                            ))),
                    ),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(product::TAGLINE_ZH),
            )
    }

    fn nav_entry(
        &self,
        destination: Destination,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let state = self.workspace.state(destination);
        let tone = state.tone();
        let selected = self.workspace.destination() == destination;
        let focused = self.destination_focus(destination).is_focused(window);

        let background = if selected {
            palette::SURFACE_SELECTED
        } else {
            palette::SURFACE
        };
        let label_color = if selected {
            palette::TEXT_PRIMARY
        } else {
            palette::TEXT_SECONDARY
        };
        let border = if focused {
            palette::ACCENT
        } else if selected {
            palette::BORDER_STRONG
        } else {
            palette::SURFACE
        };

        let index = destination.index();
        let count = self.workspace.item_count(destination);

        div()
            .id(("nav", index))
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::TIGHT))
            .h(metrics.nav_row_height())
            .px(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(background))
            .border_1()
            .border_color(theme::swatch(border))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(label_color))
            .track_focus(self.destination_focus(destination))
            .debug_selector(move || destination.debug_selector().to_owned())
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.select(Destination::ALL[index], window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    // The accent bar marks the current object; it is drawn in the
                    // row colour when the entry is not selected, so the row does
                    // not shift when the selection moves.
                    .child(
                        div()
                            .w(metrics.scale(2.0))
                            .h(metrics.scale(16.0))
                            .rounded(metrics.scale(1.0))
                            .bg(theme::swatch(if selected {
                                palette::ACCENT
                            } else {
                                background
                            })),
                    )
                    .child(components::status_dot(tone, metrics))
                    .child(
                        div()
                            .truncate()
                            .font_weight(if selected {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .child(destination.label(strings)),
                    ),
            )
            .child(if count > 0 {
                components::count_label(count, destination.count_unit(strings), metrics)
                    .into_any_element()
            } else {
                components::keycap(destination.shortcut_label(Platform::current()), metrics)
                    .into_any_element()
            })
    }

    fn rail(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut rail = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(metrics.rail_width())
            .h_full()
            .bg(theme::swatch(palette::SURFACE))
            .border_r_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(self.brand())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(2.0))
                    .px(metrics.scale(spacing::TIGHT)),
            );

        for destination in Destination::ALL {
            rail = rail.child(self.nav_entry(destination, window, cx));
        }

        rail.child(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .p(metrics.scale(spacing::TIGHT))
                .mt_auto()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(SharedString::from(copy::nav_hint(
                    strings,
                    Platform::current(),
                )))
                .child(
                    div()
                        .id("zoom.control")
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(metrics.scale(spacing::TIGHT))
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(spacing::HAIRLINE))
                        .rounded(px(radius::SMALL))
                        .border_1()
                        .border_color(theme::swatch(palette::BORDER))
                        .debug_selector(|| "zoom.control".to_owned())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.workspace.cycle_text_size();
                            cx.notify();
                        }))
                        .child(strings.zoom_control)
                        .child(
                            div()
                                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                .child(self.workspace.zoom().label()),
                        ),
                ),
        )
    }

    fn notice_strip(&self, cx: &mut Context<Self>) -> Option<Div> {
        let notice = self.workspace.notice()?;
        let metrics = self.metrics();
        let strings = self.strings();
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::TIGHT))
                .px(metrics.scale(spacing::BASE))
                .py(metrics.scale(spacing::TIGHT))
                .bg(theme::swatch(palette::SURFACE_RAISED))
                .border_b_1()
                .border_color(theme::swatch(palette::BORDER))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "notice".to_owned())
                .child(notice.text(strings))
                .child(
                    div()
                        .id("notice.dismiss")
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(2.0))
                        .rounded(px(radius::SMALL))
                        .text_color(Tone::Accent.text())
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.workspace.dismiss_notice();
                            cx.notify();
                        }))
                        .child("✕"),
                ),
        )
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let destination = self.workspace().destination();
        let state = self.workspace().state(destination);
        let mut chips = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .child(components::chip(
                state.chip_label(strings),
                state.tone(),
                metrics,
            ));
        if state == SectionState::Populated {
            chips = chips.child(components::count_label(
                self.workspace.item_count(destination),
                destination.count_unit(strings),
                metrics,
            ));
        }

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::BASE))
            .h(metrics.header_height())
            .flex_none()
            .px(metrics.scale(spacing::BASE))
            .bg(theme::swatch(palette::CANVAS))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(1.0))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(SharedString::from(format!(
                                "{} · {}",
                                strings.object_label,
                                destination.label(strings)
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.scale(spacing::SNUG))
                            .child(
                                div()
                                    .text_size(metrics.heading_size())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                    .truncate()
                                    .debug_selector(|| "header.title".to_owned())
                                    .child(destination.label(strings)),
                            )
                            .child(chips),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(destination.summary(strings)),
                    )
                    .child(
                        div()
                            .id("header.primary")
                            .debug_selector(|| "header.primary".to_owned())
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.request_primary(window, cx);
                            }))
                            .child(components::action_button(
                                self.primary_label(destination, strings),
                                self.workspace.can_act(destination),
                                metrics,
                            )),
                    ),
            )
    }

    /// The card a section shows when it holds no rows.
    fn section_card(&self, destination: Destination, state: SectionState) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        match state {
            SectionState::Planned => components::state_card(
                Tone::Planned,
                strings.planned_title,
                match destination {
                    Destination::LocalShells => strings.planned_local,
                    Destination::Hosts => strings.planned_hosts,
                    Destination::Sessions => strings.planned_sessions,
                    Destination::Transfers => strings.planned_transfers,
                },
                None,
                metrics,
            ),
            SectionState::Loading => components::state_card(
                Tone::Warning,
                strings.loading_title,
                strings.loading_body,
                None,
                metrics,
            ),
            SectionState::Failed => {
                let failure = self.workspace.failure(destination);
                components::state_card(
                    Tone::Danger,
                    strings.failed_title,
                    failure.map_or_else(
                        || strings.loading_body.to_owned(),
                        |failure| failure.reason().to_owned(),
                    ),
                    Some(SharedString::from(format!(
                        "{} · {}",
                        strings.recovery_label,
                        failure.map_or(strings.recovery_retry, |failure| failure
                            .recovery()
                            .label(strings))
                    ))),
                    metrics,
                )
            }
            SectionState::Empty => components::state_card(
                Tone::Neutral,
                match destination {
                    Destination::LocalShells => strings.empty_local_title,
                    Destination::Hosts => strings.empty_hosts_title,
                    Destination::Sessions => strings.empty_sessions_title,
                    Destination::Transfers => strings.empty_transfers_title,
                },
                match destination {
                    Destination::LocalShells => strings.empty_local_body,
                    Destination::Hosts => strings.empty_hosts_body,
                    Destination::Sessions => strings.empty_sessions_body,
                    Destination::Transfers => strings.empty_transfers_body,
                },
                Some(SharedString::from(format!(
                    "{} · {}",
                    strings.next_step,
                    self.primary_label(destination, strings)
                ))),
                metrics,
            ),
            SectionState::Populated => div().flex().flex_col().gap(metrics.scale(spacing::TIGHT)),
        }
    }

    fn rows_for(&self, destination: Destination, list: gpui::Div) -> gpui::Div {
        match destination {
            Destination::LocalShells => self.local_rows(list),
            // The hosts destination renders through `hosts_panel`; this arm keeps
            // the list untouched if a future layout routes it here.
            Destination::Hosts => list,
            Destination::Sessions => self.session_rows(list),
            Destination::Transfers => self.transfer_rows(list),
        }
    }

    fn destination_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let destination = self.workspace.destination();
        let state = self.workspace.state(destination);

        let body = if destination == Destination::LocalShells && self.pane.is_some() {
            self.terminal_panel(cx)
        } else if destination == Destination::Hosts {
            // The hosts destination renders its own empty, failed and populated
            // states: the tools, the form and the list are one panel.
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .id("content.scroll")
                .p(metrics.scale(spacing::SECTION))
                .gap(metrics.panel_gap())
                .debug_selector(|| "content".to_owned())
                .overflow_y_scroll()
                .child(self.hosts_destination(cx));
        } else {
            let card = self.section_card(destination, state);
            if state == SectionState::Populated {
                self.rows_for(destination, card)
            } else if destination == Destination::LocalShells {
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.panel_gap())
                    .child(self.shell_picker(cx))
                    .child(card)
            } else {
                card
            }
        };

        let container = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .id("content.scroll")
            .p(metrics.scale(spacing::SECTION))
            .gap(metrics.panel_gap())
            .debug_selector(|| "content".to_owned())
            .child(body);
        // A terminal fills the panel; a list of cards scrolls instead.
        if destination == Destination::LocalShells && self.pane.is_some() {
            container
        } else {
            container.overflow_y_scroll()
        }
    }

    /// The terminal and the shell it runs, filling the destination panel.
    fn terminal_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(metrics.scale(spacing::TIGHT))
            .child(self.shell_picker(cx));

        if let Some(pane) = &self.pane {
            // A shell that ended keeps its last screen, but the reason it stopped is
            // stated above it instead of being left for the user to guess.
            if let Some(failure) = pane.read(cx).state().failure() {
                column = column.child(
                    div()
                        .flex_none()
                        .px(metrics.scale(spacing::TIGHT))
                        .py(metrics.scale(spacing::HAIRLINE))
                        .rounded(px(radius::SMALL))
                        .bg(theme::swatch(palette::SURFACE_RAISED))
                        .border_1()
                        .border_color(theme::swatch(palette::BORDER))
                        .text_size(metrics.label_size())
                        .text_color(Tone::Danger.text())
                        .debug_selector(|| "terminal.failure".to_owned())
                        .child(SharedString::from(failure.to_string())),
                );
            }
            column = column.child(pane.clone());
        }
        column
    }

    /// The shell choices and the free-text path field.
    fn shell_picker(&self, cx: &mut Context<Self>) -> gpui::Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let mut row = div()
            .flex()
            .items_center()
            .flex_wrap()
            .flex_none()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "shell.picker".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.shell_picker_label),
            );

        if self.workspace.shells().is_empty() {
            row = row.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Warning.text())
                    .child(strings.shell_none_found),
            );
        }

        for (index, shell) in self.workspace.shells().iter().enumerate() {
            let selected = index == self.workspace.shell_choice();
            row = row.child(
                div()
                    .id(("shell.choice", index))
                    .debug_selector(move || format!("shell.choice.{index}"))
                    .px(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(2.0))
                    .rounded(px(radius::SMALL))
                    .bg(theme::swatch(if selected {
                        palette::ACCENT_TINT
                    } else {
                        palette::SURFACE_RAISED
                    }))
                    .border_1()
                    .border_color(theme::swatch(if selected {
                        palette::ACCENT
                    } else {
                        palette::BORDER
                    }))
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(if selected {
                        palette::ACCENT_ON_TINT
                    } else {
                        palette::TEXT_SECONDARY
                    }))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.workspace.select_shell(index);
                        cx.notify();
                    }))
                    .child(SharedString::from(shell.name().to_owned())),
            );
        }

        row = row.child(
            div()
                .id("shell.custom")
                .debug_selector(|| "shell.custom".to_owned())
                .px(metrics.scale(spacing::TIGHT))
                .py(metrics.scale(2.0))
                .rounded(px(radius::SMALL))
                .bg(theme::swatch(if self.custom_shell_open {
                    palette::SURFACE_SELECTED
                } else {
                    palette::SURFACE_RAISED
                }))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .cursor_pointer()
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.custom_shell_open = !this.custom_shell_open;
                    if this.custom_shell_open {
                        let handle = this.shell_path.read(cx).focus_handle().clone();
                        window.focus(&handle);
                    }
                    cx.notify();
                }))
                .child(strings.shell_custom_button),
        );

        if self.custom_shell_open {
            row = row.child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .debug_selector(|| "shell.path".to_owned())
                    .child(self.shell_path.clone()),
            );
        }
        row
    }

    fn local_rows(&self, list: Div) -> Div {
        let sessions: Vec<&Session> = self.workspace.local_sessions().collect();
        self.session_rows_from(list, &sessions)
    }

    fn session_rows(&self, list: Div) -> Div {
        let sessions: Vec<&Session> = self.workspace.sessions().items().iter().collect();
        self.session_rows_from(list, &sessions)
    }

    fn session_rows_from(&self, mut list: Div, sessions: &[&Session]) -> Div {
        for (index, session) in sessions.iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.session_row(session)),
            );
        }
        list
    }

    fn transfer_rows(&self, mut list: Div) -> Div {
        for (index, transfer) in self.workspace.transfers().items().iter().enumerate() {
            list = list.child(
                div()
                    .debug_selector(move || format!("row.{index}"))
                    .child(self.transfer_row(transfer)),
            );
        }
        list
    }

    fn session_row(&self, session: &Session) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale = self.workspace.locale();
        let tone = Tone::for_session_state(session.state());
        let kind = theme::session_kind_label(session.kind(), strings.local_shell_fallback);
        // A failed or lost session shows the reason the transport reported instead
        // of a generic "disconnected", so the user can act on it.
        let detail = match session.state() {
            SessionState::Disconnected(failure) | SessionState::Failed(failure) => {
                format!("{kind} · {}", theme::session_failure_text(failure))
            }
            state => format!("{kind} · {}", copy::session_state_label(state, locale)),
        };
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(components::stacked(
                        session.title().to_owned(),
                        detail,
                        metrics,
                    )),
            )
            .child(components::chip(
                copy::session_state_label(session.state(), locale),
                tone,
                metrics,
            ))
    }

    fn transfer_row(&self, transfer: &Transfer) -> Div {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let tone = Tone::for_transfer_state(transfer.state());
        components::row(metrics)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(components::status_dot(tone, metrics))
                    .child(components::stacked(
                        format!(
                            "{} {}",
                            theme::transfer_direction_label(transfer.direction()),
                            transfer.source()
                        ),
                        format!(
                            "→ {} · {}",
                            transfer.destination(),
                            copy::transfer_state_label(transfer.state(), locale)
                        ),
                        metrics,
                    )),
            )
            .child(components::chip(
                copy::transfer_state_label(transfer.state(), locale),
                tone,
                metrics,
            ))
    }

    fn status_bar(&self) -> Div {
        let metrics = self.metrics();
        let strings = self.strings();
        let locale: Locale = self.workspace.locale();
        let hosts = self.workspace.hosts().count();
        let sessions = self.workspace.sessions().count();
        let transfers = self.workspace.transfers().count();
        let counts = if hosts + sessions + transfers == 0 {
            strings.nothing_running.to_owned()
        } else {
            format!(
                "{hosts} {hosts_unit} · {sessions} {sessions_unit} · {transfers} {transfers_unit}",
                hosts_unit = strings.count_hosts,
                sessions_unit = strings.count_sessions,
                transfers_unit = strings.count_transfers
            )
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::BASE))
            .h(metrics.status_bar_height())
            .flex_none()
            .px(metrics.scale(spacing::BASE))
            .border_t_1()
            .border_color(theme::swatch(palette::BORDER))
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_MUTED))
            .child(SharedString::from(format!(
                "{} {} · {} {}",
                product::NAME,
                self.version(),
                strings.language_control,
                locale.endonym()
            )))
            .child(
                div()
                    .truncate()
                    .debug_selector(|| "status.tagline".to_owned())
                    .child(product::TAGLINE_EN),
            )
            .child(SharedString::from(counts))
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

impl Render for ShelluneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = self.metrics();
        let notice = self.notice_strip(cx);

        div()
            .id("shellune.root")
            .flex()
            .size_full()
            .bg(theme::swatch(palette::CANVAS))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .key_context("Shellune")
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &GoToLocalShells, window, cx| {
                this.select(Destination::LocalShells, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToHosts, window, cx| {
                this.select(Destination::Hosts, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToSessions, window, cx| {
                this.select(Destination::Sessions, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToTransfers, window, cx| {
                this.select(Destination::Transfers, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextDestination, window, cx| {
                this.workspace.select_next();
                let next = this.workspace.destination();
                this.select(next, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousDestination, window, cx| {
                this.workspace.select_previous();
                let previous = this.workspace.destination();
                this.select(previous, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CycleTextSize, _window, cx| {
                this.workspace.cycle_text_size();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleLanguage, _window, cx| {
                this.workspace.toggle_locale();
                let locale = this.workspace.locale();
                if let Some(pane) = this.pane.clone() {
                    pane.update(cx, |pane, cx| pane.set_locale(locale, cx));
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &DismissNotice, _window, cx| {
                this.workspace.dismiss_notice();
                cx.notify();
            }))
            .child(self.rail(window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(self.header(cx))
                    .children(notice)
                    .child(self.destination_panel(cx))
                    .child(self.status_bar()),
            )
    }
}

/// The store the shell reads when the caller does not supply one.
#[cfg(not(test))]
fn default_store() -> HostStore {
    HostStore::default_location()
}

/// A scratch store, so a test that mounts the whole shell never reads or writes the
/// developer's real data directory.
#[cfg(test)]
fn default_store() -> HostStore {
    scratch_store()
}

/// A store in a directory of its own, unique to this test process.
#[cfg(test)]
pub(crate) fn scratch_store() -> HostStore {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let unique = format!(
        "shellune-app-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    HostStore::at(std::env::temp_dir().join(unique))
}

/// The group label a host row shows, when the host belongs to a group.
///
/// A host without a group shows nothing rather than the count unit, which used to
/// print the word "host" where a group name belongs.
pub(crate) fn host_group_chip(host: &HostConfig) -> Option<String> {
    host.group()
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned)
}

/// A muted line of explanation under a row or a heading.
fn caption_line(text: String, metrics: Metrics) -> Div {
    div()
        .text_size(metrics.caption_size())
        .text_color(theme::swatch(palette::TEXT_SECONDARY))
        .child(SharedString::from(text))
}

/// One "line 3, line 7" list, for the lines a limit points at.
fn line_list(lines: &[usize]) -> String {
    lines
        .iter()
        .map(|line| format!("line {line}"))
        .collect::<Vec<String>>()
        .join(", ")
}
