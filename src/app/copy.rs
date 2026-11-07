//! User-facing copy, in English and Chinese (R12, R18).
//!
//! Every string the shell renders comes from here so that both languages are
//! complete at the same time and no view invents wording inline. The Chinese
//! column is not a literal translation of the English one: it is written for a
//! Chinese reader, and the tests only require that both are present, non-empty
//! and renderable.
//!
//! The product name, pronunciation and the two taglines stay in
//! [`crate::app::product`] because R18 fixes them independently of the UI
//! language.

use shellune_core::{SessionState, TransferState};

/// UI language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    /// English (default).
    #[default]
    English,
    /// Simplified Chinese.
    Chinese,
}

impl Locale {
    /// Both languages, in display order.
    pub const ALL: [Self; 2] = [Self::English, Self::Chinese];

    /// The string table for this language.
    pub fn strings(self) -> &'static Strings {
        match self {
            Self::English => &ENGLISH,
            Self::Chinese => &CHINESE,
        }
    }

    /// Name of the language in that language, for the toggle.
    pub fn endonym(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
        }
    }

    /// The language the window is drawn in right now.
    ///
    /// Set by the window each time it draws; small views that are not handed the
    /// workspace, such as a text field's context menu, read it here.
    pub fn current() -> Self {
        CURRENT_LOCALE.with(std::cell::Cell::get)
    }

    /// Records the language the window is being drawn in; see [`Locale::current`].
    pub fn set_current(self) {
        CURRENT_LOCALE.with(|current| current.set(self));
    }

    /// The other language.
    pub fn toggle(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|locale| *locale == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

thread_local! {
    /// The language the window is drawn in; per thread, because a test runs its window
    /// on its own thread and must not see another test's choice.
    static CURRENT_LOCALE: std::cell::Cell<Locale> = const { std::cell::Cell::new(Locale::English) };
}

/// One column of UI copy.
///
/// The struct is large but flat on purpose: a missing field is a compile error in
/// both languages at once, which is cheaper than a runtime fallback table.
#[derive(Debug)]
pub struct Strings {
    /// Navigation entry: local shells.
    pub nav_local: &'static str,
    /// One-line description of the local shells destination.
    pub nav_local_summary: &'static str,
    /// Navigation entry: saved hosts.
    pub nav_hosts: &'static str,
    /// One-line description of the hosts destination.
    pub nav_hosts_summary: &'static str,
    /// Navigation entry: open sessions.
    pub nav_sessions: &'static str,
    /// One-line description of the sessions destination.
    pub nav_sessions_summary: &'static str,
    /// Navigation entry: file transfers.
    pub nav_transfers: &'static str,
    /// One-line description of the transfers destination.
    pub nav_transfers_summary: &'static str,
    /// Empty-state title on the local shells destination.
    pub empty_local_title: &'static str,
    /// Empty-state body on the local shells destination.
    pub empty_local_body: &'static str,
    /// Empty-state title on the hosts destination.
    pub empty_hosts_title: &'static str,
    /// Empty-state body on the hosts destination.
    pub empty_hosts_body: &'static str,
    /// Empty-state title on the sessions destination.
    pub empty_sessions_title: &'static str,
    /// Empty-state body on the sessions destination.
    pub empty_sessions_body: &'static str,
    /// Empty-state title on the transfers destination.
    pub empty_transfers_title: &'static str,
    /// Empty-state body on the transfers destination.
    pub empty_transfers_body: &'static str,
    /// Chip label for an empty, loaded section.
    pub state_empty: &'static str,
    /// Chip label for a section holding real, non-empty data.
    ///
    /// It states the status, unlike the count next to it, which states the amount:
    /// a unit word such as "session" must never stand in for a status.
    pub state_ready: &'static str,
    /// Chip label for a section that is loading.
    pub state_loading: &'static str,
    /// Title shown while a section is loading.
    pub loading_title: &'static str,
    /// Body shown while a section is loading.
    pub loading_body: &'static str,
    /// Chip label for a failed section.
    pub state_failed: &'static str,
    /// Title of a failed section.
    pub failed_title: &'static str,
    /// Label introducing the suggested recovery step.
    pub recovery_label: &'static str,
    /// Recovery action: retry.
    pub recovery_retry: &'static str,
    /// Recovery action: create a host.
    pub recovery_create_host: &'static str,
    /// Recovery action: import an SSH configuration.
    pub recovery_import_config: &'static str,
    /// Recovery action: pick a shell.
    pub recovery_choose_shell: &'static str,
    /// Label introducing the next step of an empty section.
    pub next_step: &'static str,
    /// Primary action on the local shells destination.
    pub primary_local: &'static str,
    /// Primary action on the hosts destination.
    pub primary_hosts: &'static str,
    /// Primary action on the sessions destination.
    pub primary_sessions: &'static str,
    /// Primary action on the transfers destination.
    pub primary_transfers: &'static str,
    /// Feedback after an unavailable action was requested.
    pub unavailable_notice: &'static str,
    /// Label of the zoom control.
    pub zoom_control: &'static str,
    /// Label of the language toggle.
    pub language_control: &'static str,
    /// Suffix for a count of hosts.
    pub count_hosts: &'static str,
    /// Suffix for a count of sessions.
    pub count_sessions: &'static str,
    /// Suffix for a count of transfers.
    pub count_transfers: &'static str,
    /// Fallback name for a local session without a chosen shell.
    pub local_shell_fallback: &'static str,
    /// Button that reveals the free-text shell path field.
    pub shell_custom_button: &'static str,
    /// Placeholder in the free-text shell path field.
    pub shell_custom_placeholder: &'static str,
    /// Shown when no shell was found on this machine.
    pub shell_none_found: &'static str,
    /// Terminal state: the shell is running.
    pub terminal_live: &'static str,
    /// Terminal state: the shell could not be started.
    pub terminal_failed: &'static str,
    /// Terminal state: the shell exited on its own.
    pub terminal_exited: &'static str,
    /// Terminal state: the shell was killed by a signal.
    pub terminal_terminated: &'static str,
    /// Button that copies the terminal selection.
    pub terminal_copy: &'static str,
    /// Tooltip of the button that makes the terminal text smaller.
    pub terminal_smaller: &'static str,
    /// Tooltip of the button that makes the terminal text larger.
    pub terminal_larger: &'static str,
    /// Heading above the shell tiles and the shell menu.
    pub shell_launch_title: &'static str,
    /// Tooltip of the control that opens the shell menu.
    pub shell_menu_hint: &'static str,
    /// The mark on the launcher tile the main button opens.
    pub shell_default: &'static str,
    /// The second line of the custom-path launcher tile.
    pub shell_custom_hint: &'static str,
    /// The welcome page's link to the hosts destination.
    pub welcome_hosts: &'static str,
    /// The short label beside the section-switching shortcut.
    pub nav_hint_short: &'static str,
    /// The sidebar's activity line when nothing is open.
    pub idle: &'static str,
    /// Primary action on the transfers page when no connection can browse files yet.
    pub primary_transfers_connect: &'static str,
    /// Placeholder in the host search field.
    pub hosts_search_placeholder: &'static str,
    /// Host list view: every saved host.
    pub hosts_view_all: &'static str,
    /// Host list view: hosts grouped by their group label.
    pub hosts_view_groups: &'static str,
    /// Host list view: hosts that were really connected, newest first.
    pub hosts_view_recent: &'static str,
    /// Shown when no connection has been recorded yet.
    pub hosts_recent_empty: &'static str,
    /// Shown when a search matches no saved host.
    pub hosts_no_match: &'static str,
    /// Group heading for hosts without a group label.
    pub hosts_ungrouped: &'static str,
    /// Form label: host name.
    pub host_field_name: &'static str,
    /// Form label: host address.
    pub host_field_address: &'static str,
    /// Form label: TCP port.
    pub host_field_port: &'static str,
    /// Form label: login user.
    pub host_field_user: &'static str,
    /// Form label: group label.
    pub host_field_group: &'static str,
    /// Form label: the private key file a connection may use.
    pub host_field_identity: &'static str,
    /// Note that only the key path is stored, never the key.
    pub host_identity_note: &'static str,
    /// Title of the editor when a new host is being created.
    pub host_editor_new: &'static str,
    /// Title of the editor when a saved host is being changed.
    pub host_editor_edit: &'static str,
    /// Button that stores the form.
    pub host_save: &'static str,
    /// Button that closes the form without storing anything.
    pub host_cancel: &'static str,
    /// Button that opens a saved host in the editor.
    pub host_edit: &'static str,
    /// Button that starts deleting a saved host.
    pub host_delete: &'static str,
    /// Question shown before a host is deleted.
    pub host_delete_question: &'static str,
    /// Button that confirms the deletion.
    pub host_delete_confirm: &'static str,
    /// Note explaining what deleting a host does and does not touch.
    pub host_delete_note: &'static str,
    /// Feedback after a host was stored.
    pub hosts_saved: &'static str,
    /// Feedback after a host was removed.
    pub hosts_deleted: &'static str,
    /// Label introducing where the hosts are stored.
    pub hosts_storage_label: &'static str,
    /// Button that reads the host file again after a failure.
    pub hosts_retry: &'static str,
    /// Label introducing when a host was last reached.
    pub host_last_connected: &'static str,
    /// Unit after a count of successful connections.
    pub host_connection_count: &'static str,

    /// Button that opens the SSH configuration import panel.
    pub import_open: &'static str,
    /// Title of the import panel.
    pub import_title: &'static str,
    /// Label of the configuration file field.
    pub import_path_label: &'static str,
    /// Placeholder in the configuration file field.
    pub import_path_placeholder: &'static str,
    /// Button that reads the file and shows what would be imported.
    pub import_preview: &'static str,
    /// Label introducing the path the preview really read.
    pub import_source_label: &'static str,
    /// Note that the configuration file is only read.
    pub import_read_only: &'static str,
    /// Title of the limits block shown before applying.
    pub import_limits_title: &'static str,
    /// Shown when the file was applied without limitation.
    pub import_limits_none: &'static str,
    /// Label for hosts the importer understood completely.
    pub import_outcome_ready: &'static str,
    /// Label for hosts that can be imported but need a review.
    pub import_outcome_review: &'static str,
    /// Label for hosts that are not imported because their path cannot be trusted.
    pub import_outcome_blocked: &'static str,
    /// Note on a host that will not be imported.
    pub import_not_offered: &'static str,
    /// Label introducing directives this build does not apply.
    pub import_ignored_label: &'static str,
    /// Label introducing the Include lines, whose files were not read.
    pub import_include_label: &'static str,
    /// Label introducing the Match blocks, whose conditions were not evaluated.
    pub import_match_label: &'static str,
    /// Label introducing proxy or forwarding rules no host in the file matched.
    pub import_unmatched_proxy_label: &'static str,
    /// Shown when the file holds no host that could be imported.
    pub import_nothing: &'static str,
    /// Button that saves the hosts the preview offers.
    pub import_apply: &'static str,
    /// Prefix of the message left after an import.
    pub import_applied: &'static str,
    /// Unit used after the number of hosts that need a review.
    pub import_review_count: &'static str,
    /// Unit used after the number of hosts that were skipped.
    pub import_skipped: &'static str,
    /// Title of the block shown when the file could not be read or parsed.
    pub import_error_title: &'static str,
    /// Shown when the import panel was asked to read an empty path.
    pub import_path_required: &'static str,

    /// Label of the password authentication method.
    pub auth_password: &'static str,
    /// Label of the plain private key method.
    pub auth_key: &'static str,
    /// Label of the passphrase protected key method.
    pub auth_key_passphrase: &'static str,
    /// Label of the system agent method.
    pub auth_agent: &'static str,
    /// What the password field is for.
    pub auth_password_hint: &'static str,
    /// What the key path field is for.
    pub auth_key_hint: &'static str,
    /// What the passphrase field is for.
    pub auth_key_passphrase_hint: &'static str,
    /// What the agent method uses.
    pub auth_agent_hint: &'static str,
    /// Shown when the password field is empty.
    pub auth_missing_password: &'static str,
    /// Shown when the key path field is empty.
    pub auth_missing_key: &'static str,
    /// Shown when the passphrase field is empty.
    pub auth_missing_passphrase: &'static str,
    /// Button that starts a remote session.
    pub remote_connect: &'static str,
    /// Status before an attempt started.
    pub remote_idle: &'static str,
    /// Status while the transport connects.
    pub remote_connecting: &'static str,
    /// Status while the user decides about the server identity.
    pub remote_verifying: &'static str,
    /// Status of a live remote session.
    pub remote_live: &'static str,
    /// Status of a failed attempt.
    pub remote_failed: &'static str,
    /// Title of the unknown host identity question.
    pub trust_title: &'static str,
    /// Explanation shown beside the fingerprint.
    pub trust_body: &'static str,
    /// Label of the key algorithm.
    pub trust_algorithm: &'static str,
    /// Label of the SHA-256 fingerprint.
    pub trust_fingerprint: &'static str,
    /// Button that stores the identity and connects.
    pub trust_accept: &'static str,
    /// Button that refuses the identity.
    pub trust_reject: &'static str,
    /// Shown after an identity was refused.
    pub trust_rejected_note: &'static str,
    /// Shown when the attempt ended before the acceptance could reach it.
    pub trust_undelivered_note: &'static str,
    /// Title of the failed attempt panel.
    pub remote_failure_title: &'static str,
    /// Button that closes the remote panel.
    pub remote_close: &'static str,
    /// Kind label of a shell on this machine, on the session tab and bar.
    pub session_kind_local: &'static str,
    /// Kind label of an SSH session, on the session tab and bar.
    pub session_kind_remote: &'static str,
    /// Button that closes one session.
    pub session_close: &'static str,
    /// Button that starts another process for a session that ended.
    pub session_reconnect: &'static str,
    /// Why a local session cannot be restarted.
    pub session_reconnect_no_shell: &'static str,
    /// Why a remote session's host cannot be connected to again.
    pub session_reconnect_missing_host: &'static str,
    /// Said where a selected session has no terminal to draw.
    pub session_no_terminal: &'static str,

    /// Button that opens the file view of a remote session.
    pub files_open: &'static str,
    /// Button that closes the file view.
    pub files_close: &'static str,
    /// Label introducing the host a path belongs to.
    pub files_host_label: &'static str,
    /// Label introducing the current path.
    pub files_path_label: &'static str,
    /// Button that lists the parent directory.
    pub files_up: &'static str,
    /// Button that lists the current directory again.
    pub files_refresh: &'static str,
    /// Row that leads to the parent directory.
    pub files_parent_row: &'static str,
    /// Shown while a directory is being read.
    pub files_loading: &'static str,
    /// Shown when a directory is really empty.
    pub files_empty: &'static str,
    /// Shown while no path is known yet.
    pub files_unknown_path: &'static str,
    /// Shown when a directory could not be read.
    pub files_failed: &'static str,
    /// Said beside a file view failure, so the terminal is not read as failed too.
    pub files_terminal_note: &'static str,
    /// Said when this file source failed and the terminal is still connected.
    ///
    /// Refresh is the recovery: the session is not ended, so reconnect would do nothing.
    pub files_refresh_source: &'static str,
    /// Kind label of a directory entry.
    pub files_kind_directory: &'static str,
    /// Kind label of a file entry.
    pub files_kind_file: &'static str,
    /// Kind label of a symbolic link entry.
    pub files_kind_link: &'static str,
    /// Kind label of any other entry.
    pub files_kind_other: &'static str,
    /// Button that opens the new-folder form.
    pub files_new: &'static str,
    /// Button that opens the rename form of one entry.
    pub files_rename: &'static str,
    /// Button that asks the delete question about one entry.
    pub files_delete: &'static str,
    /// Placeholder of the field that names a new folder.
    pub files_new_placeholder: &'static str,
    /// Placeholder of the field that holds the new name of an entry.
    pub files_rename_placeholder: &'static str,
    /// Button that sends the new directory.
    pub files_create: &'static str,
    /// Button that sends the rename.
    pub files_rename_confirm: &'static str,
    /// Button that sends the deletion.
    pub files_delete_confirm: &'static str,
    /// Button that closes a form without sending anything.
    pub files_cancel: &'static str,
    /// Label introducing the full path a question is about.
    pub files_target_label: &'static str,
    /// Label introducing the kind of the entry a question is about.
    pub files_kind_label: &'static str,
    /// Said about a directory that is deleted with everything inside it.
    pub files_delete_recursive: &'static str,
    /// Said about a file that is deleted.
    pub files_delete_file: &'static str,
    /// Said while a change waits for the server.
    pub files_change_pending: &'static str,
    /// Heading of a change the server applied completely.
    pub files_change_done: &'static str,
    /// Heading of a change the server refused in part.
    pub files_change_failed: &'static str,
    /// Word before the count of paths a change really reached.
    pub files_change_changed: &'static str,
    /// Reason a change cannot be asked for yet.
    pub files_problem_no_directory: &'static str,
    /// Reason: the name is empty.
    pub files_problem_empty: &'static str,
    /// Reason: the name is "." or "..".
    pub files_problem_navigation: &'static str,
    /// Reason: the name holds a separator.
    pub files_problem_separator: &'static str,
    /// Reason: the name is already in the listing.
    pub files_problem_taken: &'static str,
    /// Reason: the entry is not in the listing any more.
    pub files_problem_missing: &'static str,
    /// Reason: the new name is the old one.
    pub files_problem_unchanged: &'static str,
    /// Kind label of the local file column.
    pub files_source_local: &'static str,
    /// Kind label of the remote file column.
    pub files_source_remote: &'static str,
    /// Narrow-window button that shows the local column.
    pub files_show_local: &'static str,
    /// Narrow-window button that shows the remote column.
    pub files_show_remote: &'static str,
    /// Button that uploads one local file into the remote directory on screen.
    pub files_to_remote: &'static str,
    /// Button that downloads one remote file into the local directory on screen.
    pub files_to_local: &'static str,
    /// Said about a directory deleted on this computer, including its contents.
    pub files_delete_recursive_local: &'static str,
    /// Said about a file deleted on this computer.
    pub files_delete_file_local: &'static str,
    /// Said while a local change has not been listed again.
    pub files_change_pending_local: &'static str,
    /// Heading when this computer applied a change.
    pub files_change_done_local: &'static str,
    /// Heading when this computer did not apply every part of a change.
    pub files_change_failed_local: &'static str,
    /// Local failure: the path is not there. The path is appended.
    pub files_local_not_found: &'static str,
    /// Local failure: this account may not read the path. The path is appended.
    pub files_local_denied: &'static str,
    /// Local failure: the path is not a directory. The path is appended.
    pub files_local_not_directory: &'static str,
    /// Local failure: the disk or volume went away. The reason is appended.
    pub files_local_lost: &'static str,
    /// Local failure: the computer refused the request. The reason is appended.
    pub files_local_refused: &'static str,
    /// Local failure: the name is already taken. The path is appended.
    pub files_local_exists: &'static str,
    /// Local failure: the folder cannot be listed at all. The reason is appended.
    pub files_local_unavailable: &'static str,
    /// Label of the root breadcrumb on this computer.
    pub files_local_root: &'static str,
    /// Heading of the transfer list of a session.
    pub transfers_heading: &'static str,
    /// Said when a session has no transfer yet.
    pub transfers_empty: &'static str,
    /// Button that asks for the local file to upload.
    pub transfers_upload: &'static str,
    /// Button that sends an upload.
    pub transfers_upload_confirm: &'static str,
    /// Placeholder of the field that holds the local file to upload.
    pub transfers_upload_placeholder: &'static str,
    /// Button that asks where one entry is downloaded to.
    pub transfers_download: &'static str,
    /// Button that sends a download.
    pub transfers_download_confirm: &'static str,
    /// Placeholder of the field that holds the local folder.
    pub transfers_download_placeholder: &'static str,
    /// Button that stops a transfer.
    pub transfers_cancel: &'static str,
    /// Button that starts a new task for one that did not finish.
    pub transfers_retry: &'static str,
    /// Word before the partial file a failed transfer left behind.
    pub transfers_residue: &'static str,
    /// Question asked before an existing target is replaced.
    pub transfers_overwrite_question: &'static str,
    /// Answer that replaces the existing target.
    pub transfers_overwrite_replace: &'static str,
    /// Answer that keeps the existing target.
    pub transfers_overwrite_keep: &'static str,
    /// Reason: the path is empty.
    pub transfers_problem_empty_path: &'static str,
    /// Reason: the path names no file.
    pub transfers_problem_no_name: &'static str,
    /// Reason: the source names no file.
    pub transfers_problem_no_source: &'static str,
    /// Reason: no directory is known for the target.
    pub transfers_problem_no_directory: &'static str,
    /// Reason: the connection cannot take the command.
    pub transfers_problem_no_connection: &'static str,
    /// Question asked before a session with running transfers is closed.
    pub transfers_close_question: &'static str,
    /// Answer that keeps a session open.
    pub transfers_close_cancel: &'static str,
    /// Answer that closes the session and cancels its transfers.
    pub transfers_close_confirm: &'static str,
    /// Button and hint that open the command palette.
    pub palette_open: &'static str,
    /// Short name of the palette in the shortcut hints.
    pub palette_hint_short: &'static str,
    /// Placeholder of the palette's search field.
    pub palette_placeholder: &'static str,
    /// Shown when nothing in the palette matches.
    pub palette_empty: &'static str,
    /// Palette group: open sessions.
    pub palette_group_sessions: &'static str,
    /// Palette group: shells to start.
    pub palette_group_shells: &'static str,
    /// Palette group: saved hosts.
    pub palette_group_hosts: &'static str,
    /// Palette group: pages.
    pub palette_group_navigate: &'static str,
    /// Palette group: window actions.
    pub palette_group_actions: &'static str,
    /// Palette action: close the session on screen.
    pub palette_close_tab: &'static str,
    /// Palette action: switch the interface language.
    pub palette_language: &'static str,
    /// Palette action: step the text size.
    pub palette_text_size: &'static str,
    /// Marks the session already on screen.
    pub palette_current: &'static str,
    /// Footer hint: arrow keys.
    pub palette_key_move: &'static str,
    /// Footer hint: Enter.
    pub palette_key_run: &'static str,
    /// Footer hint: Escape.
    pub palette_key_close: &'static str,
    /// Tab menu: open another session like this one.
    pub tab_menu_duplicate: &'static str,
    /// Tab menu: close this tab.
    pub tab_menu_close: &'static str,
    /// Tab menu: close every other tab.
    pub tab_menu_close_others: &'static str,
    /// Tab menu: close the tabs after this one.
    pub tab_menu_close_right: &'static str,
    /// Session menu: show the terminal.
    pub session_menu_open: &'static str,
    /// Session menu: start a closed session again.
    pub session_menu_reopen: &'static str,
    /// Session menu: close the tab, keep the record.
    pub session_menu_close: &'static str,
    /// Session menu: close if needed and remove the record.
    pub session_menu_remove: &'static str,
    /// Session menu and list: remove every closed record.
    pub session_menu_remove_closed: &'static str,
    /// Tooltip of the row's more-actions button.
    pub session_more: &'static str,
    /// List toolbar: how many closed records there are, before the count.
    pub session_closed_summary: &'static str,
    /// Text field menu: cut the selection.
    pub field_cut: &'static str,
    /// Text field menu: copy the selection.
    pub field_copy: &'static str,
    /// Text field menu: paste.
    pub field_paste: &'static str,
    /// Text field menu: select everything.
    pub field_select_all: &'static str,
    /// Tooltip: show a hidden password.
    pub field_reveal: &'static str,
    /// Tooltip: hide the password again.
    pub field_conceal: &'static str,
    /// Button beside a path field that opens the system file chooser.
    pub field_browse: &'static str,
    /// Host dialog: heading of the sign-in choice.
    pub host_auth_title: &'static str,
    /// Host dialog: label of the key passphrase field.
    pub host_secret_passphrase: &'static str,
    /// Host dialog: checkbox that keeps the password.
    pub host_remember_password: &'static str,
    /// Host dialog: checkbox that keeps the passphrase.
    pub host_remember_passphrase: &'static str,
    /// Host dialog: where a remembered secret is kept.
    pub host_remember_note: &'static str,
    /// Host dialog: placeholder of the secret field when one is saved.
    pub host_secret_saved_placeholder: &'static str,
    /// Host dialog: the system store refused the secret; the reason follows.
    pub host_secret_failed: &'static str,
    /// Host dialog: label of the startup command.
    pub host_startup_label: &'static str,
    /// Host dialog: placeholder of the startup command.
    pub host_startup_placeholder: &'static str,
    /// Host dialog: what the startup command does.
    pub host_startup_note: &'static str,
    /// Host dialog: what the agent method needs.
    pub host_agent_note: &'static str,
    /// Host dialog: save and open a connection at once.
    pub host_save_connect: &'static str,
    /// Tooltip of the dialog's close button.
    pub host_dialog_close: &'static str,
    /// Connection form: a remembered secret could not be read; the reason follows.
    pub remote_saved_secret_failed: &'static str,
    /// Sidebar button and palette action: collapse the sidebar.
    pub sidebar_collapse: &'static str,
    /// Sidebar button and palette action: expand the sidebar.
    pub sidebar_expand: &'static str,
    /// Sidebar control: the palette the window is drawn in.
    pub theme_control: &'static str,
    /// Theme value: follow the operating system.
    pub theme_system: &'static str,
    /// Theme value: light.
    pub theme_light: &'static str,
    /// Theme value: dark.
    pub theme_dark: &'static str,
    /// Palette action: step the theme.
    pub palette_theme: &'static str,
    /// Opens a connection that is not saved as a host first.
    pub quick_connect: &'static str,
    /// Explains that a quick connection does not write a host record.
    pub quick_connect_hint: &'static str,
    /// Title of the quick-connection form.
    pub quick_connect_title: &'static str,
    /// Saves a successful quick connection as a host.
    pub quick_save_host: &'static str,
    /// Shown after that host was written.
    pub quick_saved: &'static str,
    /// Shown while a quick connection has not been saved.
    pub quick_unsaved_note: &'static str,
    /// Heading above connection attempts that are not the one on screen.
    pub attempt_others: &'static str,
    /// Cancels one connection attempt.
    pub attempt_cancel: &'static str,
    /// Palette group: hosts that were really reached, newest first.
    pub palette_group_recent: &'static str,
    /// Label of the current session in the identity bar.
    pub workspace_current: &'static str,
    /// Quick connect: the port is not a number from 1 to 65535.
    pub quick_port_invalid: &'static str,
    /// Quick connect: what to show when the port field is blank.
    pub quick_port_blank: &'static str,
    /// Quick connect: the port field is the one to change.
    pub quick_fix_port: &'static str,
    /// Quick connect: a blank address left the connection with no name.
    pub quick_invalid_name: &'static str,
    /// Quick connect: the address field is empty.
    pub quick_invalid_address: &'static str,
    /// Quick connect: the address field contains whitespace.
    pub quick_invalid_address_space: &'static str,
    /// Quick connect: the user field is empty.
    pub quick_invalid_user: &'static str,
    /// Shown after a quick connection was saved, as the next step.
    pub quick_saved_next: &'static str,
    /// The host file could not be written while saving a quick connection.
    pub quick_save_write_failed: &'static str,
    /// What to do after that write failed.
    pub quick_save_write_next: &'static str,
    /// A quick connection's identifier is already a saved host.
    pub quick_save_duplicate: &'static str,
    /// What to do when that identifier is already saved.
    pub quick_save_duplicate_next: &'static str,
    /// The host to update is not in the file.
    pub quick_save_unknown: &'static str,
    /// What to do when the host is not in the file.
    pub quick_save_unknown_next: &'static str,
    /// The saved hosts disagree, so this one was not written.
    pub quick_save_inconsistent: &'static str,
    /// What to do when the saved hosts disagree.
    pub quick_save_inconsistent_next: &'static str,
    /// Host dialog: heading of the ordered jump list.
    pub host_jump_title: &'static str,
    /// Host dialog: shown when the jump list is empty.
    pub host_jump_empty: &'static str,
    /// Host dialog: adds a saved host to the jump list.
    pub host_jump_add: &'static str,
    /// Host dialog: moves a jump earlier.
    pub host_jump_up: &'static str,
    /// Host dialog: moves a jump later.
    pub host_jump_down: &'static str,
    /// Host dialog: removes a jump.
    pub host_jump_remove: &'static str,
    /// Host dialog: label of the ProxyCommand field.
    pub host_proxy_label: &'static str,
    /// Host dialog: placeholder of the ProxyCommand field.
    pub host_proxy_placeholder: &'static str,
    /// Host dialog: the command is stored and not run from the form.
    pub host_proxy_note: &'static str,
    /// Host dialog: both a jump list and a ProxyCommand are set.
    pub host_proxy_conflict: &'static str,
    /// Connection form: the node the missing credentials belong to.
    pub remote_asking_hop: &'static str,
    /// Connection form: title of the ProxyCommand confirmation.
    pub remote_proxy_title: &'static str,
    /// Connection form: run the expanded ProxyCommand.
    pub remote_proxy_confirm: &'static str,
    /// Connection form: do not run the ProxyCommand.
    pub remote_proxy_refuse: &'static str,
    /// Import preview: label in front of a ProxyCommand.
    pub import_proxy_label: &'static str,
    /// Import preview: label in front of a jump hop.
    pub import_jump_label: &'static str,
    /// Import preview: label in front of a forwarding rule.
    pub import_forward_label: &'static str,
    /// Import: hosts were written and the forwarding file was not.
    pub import_forwards_failed: &'static str,
    /// Pane: split the active pane to the right.
    pub pane_split_right: &'static str,
    /// Pane: split the active pane downward.
    pub pane_split_down: &'static str,
    /// Pane: the active pane fills its tab.
    pub pane_zoom: &'static str,
    /// Pane: leave zoom and show the other panes.
    pub pane_unzoom: &'static str,
    /// Marks the pane that receives the keyboard.
    pub pane_active: &'static str,
    /// A restored pane that has not been started.
    pub pane_pending: &'static str,
    /// Explains that a pending pane has no process and no old output.
    pub pane_pending_body: &'static str,
    /// Starts the pane the user is looking at. Nothing starts before this.
    pub pane_connect: &'static str,
    /// Removes one pane.
    pub pane_remove: &'static str,
    /// Closes one pane and leaves the others running.
    pub pane_close: &'static str,
    /// Shrinks the active pane along its split.
    pub pane_resize_less: &'static str,
    /// Grows the active pane along its split.
    pub pane_resize_more: &'static str,
    /// Moves the active pane left.
    pub pane_move_left: &'static str,
    /// Moves the active pane right.
    pub pane_move_right: &'static str,
    /// Moves the active pane up.
    pub pane_move_up: &'static str,
    /// Moves the active pane down.
    pub pane_move_down: &'static str,
    /// Tab menu: move this tab toward the start.
    pub tab_menu_move_left: &'static str,
    /// Tab menu: move this tab toward the end.
    pub tab_menu_move_right: &'static str,
    /// Shown when the last pane has been closed.
    pub workspace_empty_title: &'static str,
    /// The next step after the last pane is closed.
    pub workspace_empty_body: &'static str,
    /// A host id in a pane is not in the saved book.
    pub pane_missing_host: &'static str,
    /// What to do about a pane whose host is missing.
    pub pane_missing_next: &'static str,
    /// The saved active tab id is not in the workspace.
    pub workspace_dangling_tab: &'static str,
    /// The saved active pane id is not on its tab.
    pub workspace_dangling_pane: &'static str,
    /// Classic terminal mode, restored as a preference only.
    pub pane_mode_classic: &'static str,
    /// Command-block mode, restored as a preference. Output is not brought back.
    pub pane_mode_integrated: &'static str,
    /// Toolbar: show the classic grid.
    pub terminal_mode_classic: &'static str,
    /// Toolbar: show command blocks when the shell reports them.
    pub terminal_mode_blocks: &'static str,
    /// Command block that has not ended.
    pub terminal_block_running: &'static str,
    /// Command block whose exit code was not reported.
    pub terminal_block_unknown: &'static str,
    /// Label before a reported exit code.
    pub terminal_block_exit: &'static str,
    /// Copies one command block.
    pub terminal_block_copy: &'static str,
    /// Runs the block's command again, as a new execution.
    pub terminal_block_rerun: &'static str,
    /// Opens search of this terminal's history and command blocks.
    pub terminal_search: &'static str,
    /// Shown when a search matches nothing in this terminal.
    pub terminal_search_empty: &'static str,
    /// Moves to the next history match.
    pub terminal_search_next: &'static str,
    /// Closes the history search.
    pub terminal_search_close: &'static str,
    /// Title of the paste confirmation.
    pub terminal_paste_title: &'static str,
    /// Label for the session a paste would be sent to.
    pub terminal_paste_target: &'static str,
    /// Confirms a paste into the session named above it.
    pub terminal_paste_confirm: &'static str,
    /// Drops a paste without sending it.
    pub terminal_paste_cancel: &'static str,
    /// Unit after the number of lines in a paste.
    pub terminal_paste_lines: &'static str,
    /// Enhanced mode, but the shell has not emitted integration markers.
    pub terminal_no_blocks: &'static str,
    /// Integration was seen and then stopped being trustworthy.
    pub terminal_blocks_failed: &'static str,
    /// Filter: every command block.
    pub terminal_filter_all: &'static str,
    /// Filter: commands that have not ended.
    pub terminal_filter_running: &'static str,
    /// Filter: commands with no exit code.
    pub terminal_filter_unknown: &'static str,
    /// Filter: commands that exited non-zero.
    pub terminal_filter_failed: &'static str,
    /// Filter: commands that exited zero. Not a generic success.
    pub terminal_filter_zero: &'static str,
}

/// English copy.
pub static ENGLISH: Strings = Strings {
    nav_local: "Local shells",
    nav_local_summary: "A shell on this machine",
    nav_hosts: "Hosts",
    nav_hosts_summary: "Saved servers and SSH settings",
    nav_sessions: "Sessions",
    nav_sessions_summary: "Every open terminal",
    nav_transfers: "Transfers",
    nav_transfers_summary: "Uploads and downloads",
    empty_local_title: "No local shell is open",
    empty_local_body: "A local shell runs in the current directory of this machine and needs no network.",
    empty_hosts_title: "No saved hosts yet",
    empty_hosts_body: "A saved host keeps the address, port and user so a connection does not have to be retyped.",
    empty_sessions_title: "No session is open",
    empty_sessions_body: "Local and remote terminals appear here with their own name, type and state.",
    empty_transfers_title: "No transfer yet",
    empty_transfers_body: "Uploads and downloads list their source, target and progress here.",
    state_empty: "Empty",
    state_ready: "Ready",
    state_loading: "Loading",
    loading_title: "Waiting for real data",
    loading_body: "Nothing is shown here until the system reports what actually exists.",
    state_failed: "Failed",
    failed_title: "This could not be completed",
    recovery_label: "Suggested next step",
    recovery_retry: "Try again",
    recovery_create_host: "Create a host",
    recovery_import_config: "Import an SSH configuration",
    recovery_choose_shell: "Choose a shell",
    next_step: "Next step",
    primary_local: "New local shell",
    primary_hosts: "New host",
    primary_sessions: "New local shell",
    primary_transfers: "Browse files",
    unavailable_notice: "This action is not available in this build, so nothing was started.",
    zoom_control: "Text size",
    language_control: "Language",
    count_hosts: "host",
    count_sessions: "session",
    count_transfers: "transfer",
    local_shell_fallback: "System shell",
    shell_custom_button: "Custom path",
    shell_custom_placeholder: "Program name or full path, then Enter",
    shell_none_found: "No shell was found on this machine. Enter a program path to start one.",
    terminal_live: "Running",
    terminal_failed: "Could not start",
    terminal_exited: "Exited",
    terminal_terminated: "Terminated",
    terminal_copy: "Copy",
    terminal_smaller: "Smaller text",
    terminal_larger: "Larger text",
    shell_launch_title: "Start a shell",
    shell_menu_hint: "Choose which shell to open",
    shell_default: "Default",
    shell_custom_hint: "Any program, by path",
    welcome_hosts: "Connect to a server",
    nav_hint_short: "Switch sections",
    idle: "Idle",
    primary_transfers_connect: "Connect to a host",
    hosts_search_placeholder: "Search name, address, user or group",
    hosts_view_all: "All hosts",
    hosts_view_groups: "Groups",
    hosts_view_recent: "Recent connections",
    hosts_recent_empty: "No connection has been recorded yet. A host appears here after it really connected.",
    hosts_no_match: "No saved host matches this search.",
    hosts_ungrouped: "Ungrouped",
    host_field_name: "Name",
    host_field_address: "Address",
    host_field_port: "Port",
    host_field_user: "User",
    host_field_group: "Group",
    host_field_identity: "Key file",
    host_identity_note: "Only the path is saved; Shellune never reads or copies the key itself.",
    host_editor_new: "New host",
    host_editor_edit: "Edit host",
    host_save: "Save host",
    host_cancel: "Cancel",
    host_edit: "Edit",
    host_delete: "Delete",
    host_delete_question: "Delete this saved host?",
    host_delete_confirm: "Delete host",
    host_delete_note: "Only the saved record is removed; nothing on the server changes.",
    hosts_saved: "Host saved. No connection was opened.",
    hosts_deleted: "Host deleted.",
    hosts_storage_label: "Saved in",
    hosts_retry: "Read the file again",
    host_last_connected: "Last connected",
    host_connection_count: "connections",

    import_open: "Import SSH config",
    import_title: "Import an SSH configuration",
    import_path_label: "File",
    import_path_placeholder: "Path to an SSH config file",
    import_preview: "Preview",
    import_source_label: "Read from",
    import_read_only: "The file is only read; Shellune never writes to it.",
    import_limits_title: "Before importing",
    import_limits_none: "Every host in this file was read completely.",
    import_outcome_ready: "Importable",
    import_outcome_review: "Needs review",
    import_outcome_blocked: "Blocked",
    import_not_offered: "not imported",
    import_ignored_label: "Not applied",
    import_include_label: "Included files were not read",
    import_match_label: "Match blocks were not evaluated",
    import_unmatched_proxy_label: "Proxy or forwarding rules no host here matches",
    import_nothing: "No host in this file can be imported as a direct connection yet.",
    import_apply: "Import hosts",
    import_applied: "Imported",
    import_review_count: "need review",
    import_skipped: "skipped",
    import_error_title: "Nothing was imported",
    import_path_required: "Name the configuration file to read.",
    auth_password: "Password",
    auth_key: "Key file",
    auth_key_passphrase: "Key file with passphrase",
    auth_agent: "System agent",
    auth_password_hint: "Used for this attempt only; Shellune never saves it.",
    auth_key_hint: "A readable private key the server authorizes.",
    auth_key_passphrase_hint: "The passphrase decrypts the key for this attempt only.",
    auth_agent_hint: "Signs with a key the system agent already holds.",
    auth_missing_password: "Enter the password for this host.",
    auth_missing_key: "Choose the private key file to use.",
    auth_missing_passphrase: "Enter the passphrase for this key.",
    remote_connect: "Connect",
    remote_idle: "Not connected yet",
    remote_connecting: "Connecting",
    remote_verifying: "Waiting for your decision",
    remote_live: "SSH session live",
    remote_failed: "Could not connect",
    trust_title: "This server identity is not known yet",
    trust_body: "Compare this fingerprint with the one the server's administrator can print. Nothing is stored until you accept it.",
    trust_algorithm: "Algorithm",
    trust_fingerprint: "SHA-256 fingerprint",
    trust_accept: "Accept and connect",
    trust_reject: "Reject",
    trust_rejected_note: "The identity was rejected, so no terminal was opened and no trust record was written.",
    trust_undelivered_note: "The connection attempt had already ended, so the identity was neither accepted nor stored. Connect again to see it once more.",
    remote_failure_title: "The connection did not open",
    remote_close: "Close the connection panel",
    session_kind_local: "Local",
    session_kind_remote: "SSH",
    session_close: "Close session",
    session_reconnect: "Reconnect",
    session_reconnect_no_shell: "This session has no program recorded, so it cannot be started again.",
    session_reconnect_missing_host: "The saved host this session used is no longer there, so it cannot be connected to again.",
    session_no_terminal: "This session has no terminal open",
    files_open: "Files",
    files_close: "Close files",
    files_host_label: "Host",
    files_path_label: "Path",
    files_up: "Up",
    files_refresh: "Refresh",
    files_parent_row: "Parent directory",
    files_loading: "Reading the directory…",
    files_empty: "This directory is empty.",
    files_unknown_path: "Waiting for the server",
    files_failed: "The directory could not be read",
    files_terminal_note: "The terminal keeps running. Only this source is affected.",
    files_refresh_source: "Refresh reads this source again. The terminal stays connected.",
    files_kind_directory: "Directory",
    files_kind_file: "File",
    files_kind_link: "Link",
    files_kind_other: "Other",
    files_new: "New folder",
    files_rename: "Rename",
    files_delete: "Delete",
    files_new_placeholder: "Folder name",
    files_rename_placeholder: "New name",
    files_create: "Create",
    files_rename_confirm: "Rename",
    files_delete_confirm: "Delete",
    files_cancel: "Cancel",
    files_target_label: "Target",
    files_kind_label: "Type",
    files_delete_recursive: "This directory and everything inside it are deleted on the server. This cannot be undone.",
    files_delete_file: "This file is deleted on the server. This cannot be undone.",
    files_change_pending: "Waiting for the server\u{2026}",
    files_change_done: "The server applied this change",
    files_change_failed: "The server did not apply every part of this change",
    files_change_changed: "changed",
    files_problem_no_directory: "Open a directory before changing it.",
    files_problem_empty: "A name cannot be empty.",
    files_problem_navigation: "The names . and .. are navigation, not names.",
    files_problem_separator: "A name cannot contain a separator; create one directory at a time.",
    files_problem_taken: "An entry with that name already exists here.",
    files_problem_missing: "That entry is no longer in this directory.",
    files_problem_unchanged: "The name did not change.",
    files_source_local: "Local",
    files_source_remote: "Remote",
    files_show_local: "Show local",
    files_show_remote: "Show remote",
    files_to_remote: "Upload to remote",
    files_to_local: "Download to local",
    files_delete_recursive_local: "This directory and everything inside it are deleted on this computer. This cannot be undone.",
    files_delete_file_local: "This file is deleted on this computer. This cannot be undone.",
    files_change_pending_local: "Waiting for this computer\u{2026}",
    files_change_done_local: "This computer applied this change",
    files_change_failed_local: "This computer did not apply every part of this change",
    files_local_not_found: "This computer has no such path:",
    files_local_denied: "This computer refused access to",
    files_local_not_directory: "This path is not a directory:",
    files_local_lost: "This computer could not be read:",
    files_local_refused: "This computer refused the request:",
    files_local_exists: "That name is already on this computer:",
    files_local_unavailable: "This folder cannot be listed:",
    files_local_root: "This computer",
    transfers_heading: "Transfers",
    transfers_empty: "Nothing is being moved for this session.",
    transfers_upload: "Upload",
    transfers_upload_confirm: "Upload",
    transfers_upload_placeholder: "Local file, for example C:\\work\\report.txt",
    transfers_download: "Download",
    transfers_download_confirm: "Download",
    transfers_download_placeholder: "Local folder",
    transfers_cancel: "Cancel",
    transfers_retry: "Retry",
    transfers_residue: "Partial file left behind",
    transfers_overwrite_question: "This target already exists",
    transfers_overwrite_replace: "Replace it",
    transfers_overwrite_keep: "Keep the existing one",
    transfers_problem_empty_path: "Enter a path.",
    transfers_problem_no_name: "The path must end with a file name.",
    transfers_problem_no_source: "The source names no file.",
    transfers_problem_no_directory: "Open a directory first.",
    transfers_problem_no_connection: "The connection cannot take a transfer right now.",
    transfers_close_question: "Closing this session cancels its transfers",
    transfers_close_cancel: "Keep the session open",
    transfers_close_confirm: "Close and cancel",
    palette_open: "Search…",
    palette_hint_short: "Command palette",
    palette_placeholder: "Search sessions, shells, hosts and actions",
    palette_empty: "Nothing matches",
    palette_group_sessions: "Open sessions",
    palette_group_shells: "New local shell",
    palette_group_hosts: "Connect to host",
    palette_group_navigate: "Go to",
    palette_group_actions: "Actions",
    palette_close_tab: "Close current session",
    palette_language: "Switch interface language",
    palette_text_size: "Change text size",
    palette_current: "Current",
    palette_key_move: "Select",
    palette_key_run: "Open",
    palette_key_close: "Close",
    tab_menu_duplicate: "Duplicate tab",
    tab_menu_close: "Close tab",
    tab_menu_close_others: "Close other tabs",
    tab_menu_close_right: "Close tabs to the right",
    session_menu_open: "Open",
    session_menu_reopen: "Reopen",
    session_menu_close: "Close",
    session_menu_remove: "Remove",
    session_menu_remove_closed: "Remove all closed",
    session_more: "More actions",
    session_closed_summary: "Closed sessions",
    field_cut: "Cut",
    field_copy: "Copy",
    field_paste: "Paste",
    field_select_all: "Select all",
    field_reveal: "Show",
    field_conceal: "Hide",
    field_browse: "Browse…",
    host_auth_title: "Sign in with",
    host_secret_passphrase: "Key passphrase",
    host_remember_password: "Remember password",
    host_remember_passphrase: "Remember passphrase",
    host_remember_note: "Kept in the system credential store (Keychain or Credential Manager), never in the host file.",
    host_secret_saved_placeholder: "Saved; leave empty to keep it",
    host_secret_failed: "The password was not saved to the system credential store",
    host_startup_label: "Run after connecting",
    host_startup_placeholder: "For example cd /srv && tmux attach",
    host_startup_note: "Typed into the remote shell once, each time the connection opens.",
    host_agent_note: "Uses a key already loaded into the system SSH agent; nothing to fill in here.",
    host_save_connect: "Save and connect",
    host_dialog_close: "Close",
    remote_saved_secret_failed: "The saved password could not be read",
    sidebar_collapse: "Collapse sidebar",
    sidebar_expand: "Expand sidebar",
    theme_control: "Theme",
    theme_system: "System",
    theme_light: "Light",
    theme_dark: "Dark",
    palette_theme: "Change theme",
    quick_connect: "Quick connect",
    quick_connect_hint: "Connect now. The host is saved only if you ask.",
    quick_connect_title: "Quick connect",
    quick_save_host: "Save as host",
    quick_saved: "Saved as a host",
    quick_unsaved_note: "This connection is not a saved host yet",
    attempt_others: "Other connections",
    attempt_cancel: "Cancel",
    palette_group_recent: "Recent connections",
    workspace_current: "Current",
    quick_port_invalid: "The port is not a whole number from 1 to 65535",
    quick_port_blank: "empty",
    quick_fix_port: "Change the Port field.",
    quick_invalid_name: "The address is empty, so this connection has no name. Change the Address field.",
    quick_invalid_address: "The address is empty. Change the Address field.",
    quick_invalid_address_space: "The address contains a space. Change the Address field so it has none.",
    quick_invalid_user: "The user name is empty. Change the User field.",
    quick_saved_next: "Open it later from the host list. This session stays connected.",
    quick_save_write_failed: "The host file could not be written, so this connection was not saved.",
    quick_save_write_next: "This session stays connected. Make the host file writable, then save again.",
    quick_save_duplicate: "A host with this identifier is already saved, so nothing was written.",
    quick_save_duplicate_next: "Open that saved host to change it, or leave this connection unsaved.",
    quick_save_unknown: "That host is not in the saved file, so it could not be updated.",
    quick_save_unknown_next: "Save it as a new host, or check the host file. This session stays connected.",
    quick_save_inconsistent: "The saved hosts disagree with each other, so this one was not written.",
    quick_save_inconsistent_next: "Repair the host file, then save again. This session stays connected.",
    host_jump_title: "Jump hosts",
    host_jump_empty: "Direct connection. Add a saved host to reach this one through it.",
    host_jump_add: "Add",
    host_jump_up: "Move up",
    host_jump_down: "Move down",
    host_jump_remove: "Remove",
    host_proxy_label: "Proxy command",
    host_proxy_placeholder: "For example nc -X connect -x proxy:1080 %h %p",
    host_proxy_note: "Stored as text. It runs only after you confirm the expanded command.",
    host_proxy_conflict: "This host has both a jump chain and a ProxyCommand. Keep one before connecting.",
    remote_asking_hop: "Sign in to",
    remote_proxy_title: "Confirm ProxyCommand",
    remote_proxy_confirm: "Run command",
    remote_proxy_refuse: "Do not run",
    import_proxy_label: "Proxy command",
    import_jump_label: "Jump",
    import_forward_label: "Forward",
    import_forwards_failed: "Hosts were saved, but the forwarding rules were not written.",
    pane_split_right: "Split right",
    pane_split_down: "Split down",
    pane_zoom: "Zoom pane",
    pane_unzoom: "Show all panes",
    pane_active: "Active",
    pane_pending: "Not connected",
    pane_pending_body: "This pane is waiting. Nothing was started, and earlier output is not shown.",
    pane_connect: "Connect",
    pane_remove: "Remove pane",
    pane_close: "Close pane",
    pane_resize_less: "Shrink pane",
    pane_resize_more: "Grow pane",
    pane_move_left: "Move pane left",
    pane_move_right: "Move pane right",
    pane_move_up: "Move pane up",
    pane_move_down: "Move pane down",
    tab_menu_move_left: "Move tab left",
    tab_menu_move_right: "Move tab right",
    workspace_empty_title: "No pane is open",
    workspace_empty_body: "Next step: open a local shell, or connect to a host. Other sessions were not closed with this one.",
    pane_missing_host: "This host is not saved",
    pane_missing_next: "Fix the host or remove the pane. Nothing was connected.",
    workspace_dangling_tab: "The saved active tab is not in this workspace. Choose a tab. The others were kept.",
    workspace_dangling_pane: "The saved active pane is not on its tab. Choose a pane. The layout was kept.",
    pane_mode_classic: "Classic terminal",
    pane_mode_integrated: "Command blocks, not restored",
    terminal_mode_classic: "Classic grid",
    terminal_mode_blocks: "Command blocks",
    terminal_block_running: "Running",
    terminal_block_unknown: "Unknown",
    terminal_block_exit: "Exit",
    terminal_block_copy: "Copy block",
    terminal_block_rerun: "Run again",
    terminal_search: "Search this terminal",
    terminal_search_empty: "No matches in this terminal",
    terminal_search_next: "Next match",
    terminal_search_close: "Close search",
    terminal_paste_title: "Confirm paste",
    terminal_paste_target: "Target session",
    terminal_paste_confirm: "Paste into this session",
    terminal_paste_cancel: "Do not paste",
    terminal_paste_lines: "lines",
    terminal_no_blocks: "This shell has not reported command blocks. The grid is unchanged.",
    terminal_blocks_failed: "Shell integration failed. The classic grid was kept.",
    terminal_filter_all: "All commands",
    terminal_filter_running: "Running only",
    terminal_filter_unknown: "Unknown exit",
    terminal_filter_failed: "Non-zero exit",
    terminal_filter_zero: "Exit 0 only",
};

/// Chinese copy.
pub static CHINESE: Strings = Strings {
    nav_local: "本地终端",
    nav_local_summary: "在本机上运行的 Shell",
    nav_hosts: "主机",
    nav_hosts_summary: "已保存的服务器与 SSH 设置",
    nav_sessions: "会话",
    nav_sessions_summary: "所有已打开的终端",
    nav_transfers: "传输",
    nav_transfers_summary: "上传与下载",
    empty_local_title: "还没有打开本地终端",
    empty_local_body: "本地终端在当前机器的当前目录运行，不需要网络。",
    empty_hosts_title: "还没有保存的主机",
    empty_hosts_body: "保存主机后会记住地址、端口和用户名，下次连接不必重新输入。",
    empty_sessions_title: "还没有会话",
    empty_sessions_body: "本地与远程终端会各自显示名称、类型和当前状态。",
    empty_transfers_title: "还没有传输任务",
    empty_transfers_body: "上传与下载会在这里显示源、目标和进度。",
    state_empty: "空",
    state_ready: "已就绪",
    state_loading: "加载中",
    loading_title: "正在等待真实数据",
    loading_body: "在系统返回实际结果之前，这里不会显示任何内容。",
    state_failed: "失败",
    failed_title: "这一步没能完成",
    recovery_label: "建议的下一步",
    recovery_retry: "重试",
    recovery_create_host: "新建主机",
    recovery_import_config: "导入 SSH 配置",
    recovery_choose_shell: "选择 Shell",
    next_step: "下一步",
    primary_local: "新建本地终端",
    primary_hosts: "新建主机",
    primary_sessions: "新建本地终端",
    primary_transfers: "浏览远程文件",
    unavailable_notice: "当前版本没有实现这个操作，因此没有启动任何任务。",
    zoom_control: "文字大小",
    language_control: "语言",
    count_hosts: "台主机",
    count_sessions: "个会话",
    count_transfers: "个传输",
    local_shell_fallback: "系统 Shell",
    shell_custom_button: "自定义路径",
    shell_custom_placeholder: "输入程序名或完整路径后按回车",
    shell_none_found: "本机没有找到可用的 Shell。请输入程序路径来启动一个。",
    terminal_live: "运行中",
    terminal_failed: "启动失败",
    terminal_exited: "已退出",
    terminal_terminated: "被终止",
    terminal_copy: "复制",
    terminal_smaller: "缩小字号",
    terminal_larger: "放大字号",
    shell_launch_title: "启动一个 Shell",
    shell_menu_hint: "选择要打开的 Shell",
    shell_default: "默认",
    shell_custom_hint: "本机上的任意程序",
    welcome_hosts: "连接到服务器",
    nav_hint_short: "切换分区",
    idle: "空闲",
    primary_transfers_connect: "连接主机",
    hosts_search_placeholder: "搜索名称、地址、用户名或分组",
    hosts_view_all: "全部主机",
    hosts_view_groups: "按分组",
    hosts_view_recent: "最近连接",
    hosts_recent_empty: "还没有连接记录。主机真正连接成功之后才会出现在这里。",
    hosts_no_match: "没有符合这个搜索条件的主机。",
    hosts_ungrouped: "未分组",
    host_field_name: "名称",
    host_field_address: "地址",
    host_field_port: "端口",
    host_field_user: "用户名",
    host_field_group: "分组",
    host_field_identity: "密钥文件",
    host_identity_note: "只保存密钥路径，Shellune 从不读取或复制密钥内容。",
    host_editor_new: "新建主机",
    host_editor_edit: "编辑主机",
    host_save: "保存主机",
    host_cancel: "取消",
    host_edit: "编辑",
    host_delete: "删除",
    host_delete_question: "要删除这台已保存的主机吗？",
    host_delete_confirm: "删除主机",
    host_delete_note: "只删除本地保存的记录，服务器上的内容不会改变。",
    hosts_saved: "主机已保存，没有打开任何连接。",
    hosts_deleted: "主机已删除。",
    hosts_storage_label: "保存位置",
    hosts_retry: "重新读取文件",
    host_last_connected: "最近连接",
    host_connection_count: "次连接",

    import_open: "导入 SSH 配置",
    import_title: "导入 SSH 配置",
    import_path_label: "配置文件",
    import_path_placeholder: "SSH 配置文件的路径",
    import_preview: "预览",
    import_source_label: "读取自",
    import_read_only: "只读取这个文件，不会写回原配置。",
    import_limits_title: "导入前的限制",
    import_limits_none: "这个文件里的每台主机都已完整读取。",
    import_outcome_ready: "可直接导入",
    import_outcome_review: "需要复核",
    import_outcome_blocked: "已阻止",
    import_not_offered: "不导入",
    import_ignored_label: "未应用的指令",
    import_include_label: "没有读取被包含的文件",
    import_match_label: "没有评估 Match 条件块",
    import_unmatched_proxy_label: "没有匹配到任何主机的代理或转发规则",
    import_nothing: "这个文件里还没有可以直接导入的主机。",
    import_apply: "导入主机",
    import_applied: "已导入",
    import_review_count: "需复核",
    import_skipped: "已跳过",
    import_error_title: "没有导入任何内容",
    import_path_required: "请先填写要读取的配置文件路径。",
    auth_password: "密码",
    auth_key: "私钥文件",
    auth_key_passphrase: "带口令的私钥",
    auth_agent: "系统 Agent",
    auth_password_hint: "仅用于本次连接，Shellune 不会保存。",
    auth_key_hint: "服务器已授权、且本账号可读取的私钥。",
    auth_key_passphrase_hint: "口令仅用于本次解密该私钥。",
    auth_agent_hint: "使用系统 Agent 中已有的密钥完成签名。",
    auth_missing_password: "请输入该主机的密码。",
    auth_missing_key: "请选择要使用的私钥文件。",
    auth_missing_passphrase: "请输入该私钥的口令。",
    remote_connect: "连接",
    remote_idle: "尚未连接",
    remote_connecting: "正在连接",
    remote_verifying: "等待你确认服务器身份",
    remote_live: "SSH 会话已连接",
    remote_failed: "连接未建立",
    trust_title: "尚未信任该服务器身份",
    trust_body: "请与服务器管理员可打印的指纹核对；在你接受之前不会写入任何记录。",
    trust_algorithm: "算法",
    trust_fingerprint: "SHA-256 指纹",
    trust_accept: "接受并连接",
    trust_reject: "拒绝",
    trust_rejected_note: "已拒绝该身份，未打开终端，也未写入信任记录。",
    trust_undelivered_note: "连接尝试已结束，该身份既未被接受也未被记录。请重新连接以再次查看。",
    remote_failure_title: "连接没有建立",
    remote_close: "关闭连接面板",
    session_kind_local: "本地",
    session_kind_remote: "SSH 远程",
    session_close: "关闭会话",
    session_reconnect: "重新连接",
    session_reconnect_no_shell: "该会话没有记录可启动的程序，无法重新启动。",
    session_reconnect_missing_host: "该会话使用的主机记录已被删除，无法再次连接。",
    session_no_terminal: "该会话当前没有终端",
    files_open: "文件",
    files_close: "关闭文件视图",
    files_host_label: "主机",
    files_path_label: "当前路径",
    files_up: "上级",
    files_refresh: "刷新",
    files_parent_row: "上级目录",
    files_loading: "正在读取目录…",
    files_empty: "这个目录是空的。",
    files_unknown_path: "等待服务器返回",
    files_failed: "无法读取这个目录",
    files_terminal_note: "终端仍在运行，只有这个来源受影响。",
    files_refresh_source: "刷新会重新读取这个来源，终端保持连接。",
    files_kind_directory: "目录",
    files_kind_file: "文件",
    files_kind_link: "链接",
    files_kind_other: "其他",
    files_new: "新建文件夹",
    files_rename: "重命名",
    files_delete: "删除",
    files_new_placeholder: "文件夹名称",
    files_rename_placeholder: "新名称",
    files_create: "创建",
    files_rename_confirm: "重命名",
    files_delete_confirm: "删除",
    files_cancel: "取消",
    files_target_label: "目标",
    files_kind_label: "类型",
    files_delete_recursive: "这个目录及其中的全部内容都会在服务器上删除，且无法撤销。",
    files_delete_file: "这个文件会在服务器上删除，且无法撤销。",
    files_change_pending: "等待服务器\u{2026}",
    files_change_done: "服务器已应用这次修改",
    files_change_failed: "服务器没有完成这次修改的全部内容",
    files_change_changed: "已修改",
    files_problem_no_directory: "请先打开一个目录再修改。",
    files_problem_empty: "名称不能为空。",
    files_problem_navigation: "“.” 和 “..” 是导航，不是名称。",
    files_problem_separator: "名称不能包含分隔符；请逐级创建目录。",
    files_problem_taken: "这里已经有同名条目。",
    files_problem_missing: "这个条目已不在当前目录中。",
    files_problem_unchanged: "名称没有变化。",
    files_source_local: "本地",
    files_source_remote: "远程",
    files_show_local: "显示本地",
    files_show_remote: "显示远程",
    files_to_remote: "上传到远程",
    files_to_local: "下载到本地",
    files_delete_recursive_local: "这个目录及其中的全部内容都会从这台电脑删除，且无法撤销。",
    files_delete_file_local: "这个文件会从这台电脑删除，且无法撤销。",
    files_change_pending_local: "正在等待这台电脑\u{2026}",
    files_change_done_local: "这台电脑已应用这次修改",
    files_change_failed_local: "这台电脑没有完成这次修改的全部内容",
    files_local_not_found: "这台电脑上没有这个路径：",
    files_local_denied: "这台电脑拒绝访问",
    files_local_not_directory: "这个路径不是目录：",
    files_local_lost: "无法读取这台电脑：",
    files_local_refused: "这台电脑拒绝了请求：",
    files_local_exists: "这台电脑上已经有这个名称：",
    files_local_unavailable: "无法列出这个文件夹：",
    files_local_root: "这台电脑",
    transfers_heading: "传输",
    transfers_empty: "这个会话当前没有传输。",
    transfers_upload: "上传",
    transfers_upload_confirm: "上传",
    transfers_upload_placeholder: "本地文件，例如 C:\\work\\报告.txt",
    transfers_download: "下载",
    transfers_download_confirm: "下载",
    transfers_download_placeholder: "本地目录",
    transfers_cancel: "取消",
    transfers_retry: "重试",
    transfers_residue: "残留的部分文件",
    transfers_overwrite_question: "目标已经存在",
    transfers_overwrite_replace: "覆盖它",
    transfers_overwrite_keep: "保留原文件",
    transfers_problem_empty_path: "请输入路径。",
    transfers_problem_no_name: "路径必须以文件名结尾。",
    transfers_problem_no_source: "源路径没有文件名。",
    transfers_problem_no_directory: "请先打开一个目录。",
    transfers_problem_no_connection: "连接当前无法接受传输。",
    transfers_close_question: "关闭这个会话会取消它的传输",
    transfers_close_cancel: "保持会话打开",
    transfers_close_confirm: "关闭并取消",
    palette_open: "搜索…",
    palette_hint_short: "命令面板",
    palette_placeholder: "搜索会话、Shell、主机与操作",
    palette_empty: "没有匹配的结果",
    palette_group_sessions: "已打开的会话",
    palette_group_shells: "新建本地终端",
    palette_group_hosts: "连接到主机",
    palette_group_navigate: "前往",
    palette_group_actions: "操作",
    palette_close_tab: "关闭当前会话",
    palette_language: "切换界面语言",
    palette_text_size: "调整文字大小",
    palette_current: "当前",
    palette_key_move: "选择",
    palette_key_run: "打开",
    palette_key_close: "关闭",
    tab_menu_duplicate: "复制标签",
    tab_menu_close: "关闭标签",
    tab_menu_close_others: "关闭其他标签",
    tab_menu_close_right: "关闭右侧标签",
    session_menu_open: "打开",
    session_menu_reopen: "重新打开",
    session_menu_close: "关闭",
    session_menu_remove: "删除",
    session_menu_remove_closed: "清除所有已关闭",
    session_more: "更多操作",
    session_closed_summary: "已关闭的会话",
    field_cut: "剪切",
    field_copy: "复制",
    field_paste: "粘贴",
    field_select_all: "全选",
    field_reveal: "显示",
    field_conceal: "隐藏",
    field_browse: "浏览…",
    host_auth_title: "登录方式",
    host_secret_passphrase: "私钥口令",
    host_remember_password: "记住密码",
    host_remember_passphrase: "记住口令",
    host_remember_note: "保存在系统凭据库（钥匙串 / 凭据管理器），不会写入主机文件。",
    host_secret_saved_placeholder: "已保存；留空则保持不变",
    host_secret_failed: "密码没有保存到系统凭据库",
    host_startup_label: "连接后运行",
    host_startup_placeholder: "例如 cd /srv && tmux attach",
    host_startup_note: "每次连接成功后，在远程 Shell 中自动执行一次。",
    host_agent_note: "使用系统 SSH Agent 中已加载的密钥，这里无需填写。",
    host_save_connect: "保存并连接",
    host_dialog_close: "关闭",
    remote_saved_secret_failed: "无法读取已保存的密码",
    sidebar_collapse: "收起侧边栏",
    sidebar_expand: "展开侧边栏",
    theme_control: "主题",
    theme_system: "跟随系统",
    theme_light: "浅色",
    theme_dark: "深色",
    palette_theme: "切换主题",
    quick_connect: "快速连接",
    quick_connect_hint: "先连接，只有你要求时才保存为主机。",
    quick_connect_title: "快速连接",
    quick_save_host: "保存为主机",
    quick_saved: "已保存为主机",
    quick_unsaved_note: "这次连接还没有保存为主机",
    attempt_others: "其他连接",
    attempt_cancel: "取消",
    palette_group_recent: "最近连接",
    workspace_current: "当前",
    quick_port_invalid: "端口必须是 1 到 65535 的整数",
    quick_port_blank: "空的",
    quick_fix_port: "请修改端口这一栏。",
    quick_invalid_name: "地址是空的，这次连接没有名称。请修改地址这一栏。",
    quick_invalid_address: "地址是空的。请修改地址这一栏。",
    quick_invalid_address_space: "地址里含有空格。请修改地址这一栏，去掉其中的空格。",
    quick_invalid_user: "用户名是空的。请修改用户这一栏。",
    quick_saved_next: "之后可以从主机列表再次打开。当前会话保持连接。",
    quick_save_write_failed: "主机文件没有写入，所以这次连接没有保存成主机。",
    quick_save_write_next: "当前会话保持连接。让主机文件可以写入后，再保存一次。",
    quick_save_duplicate: "已经有一台主机使用了这个标识，所以没有再写入。",
    quick_save_duplicate_next: "打开已保存的那台主机再修改，或者让这次连接保持未保存。",
    quick_save_unknown: "主机文件里没有这一台，所以无法更新它。",
    quick_save_unknown_next: "可以把它另存为一台新主机，或先检查主机文件。当前会话保持连接。",
    quick_save_inconsistent: "已保存的主机记录互相不一致，所以这一台没有写入。",
    quick_save_inconsistent_next: "先修好主机文件，再保存一次。当前会话保持连接。",
    host_jump_title: "跳板主机",
    host_jump_empty: "直接连接。添加已保存的主机，即可经它到达这一台。",
    host_jump_add: "添加",
    host_jump_up: "上移",
    host_jump_down: "下移",
    host_jump_remove: "移除",
    host_proxy_label: "代理命令",
    host_proxy_placeholder: "例如 nc -X connect -x proxy:1080 %h %p",
    host_proxy_note: "只保存命令文本。展开后的命令需要你确认才会执行。",
    host_proxy_conflict: "这台主机同时有跳板链和 ProxyCommand。连接前只保留一种。",
    remote_asking_hop: "登录到",
    remote_proxy_title: "确认代理命令",
    remote_proxy_confirm: "运行命令",
    remote_proxy_refuse: "不运行",
    import_proxy_label: "代理命令",
    import_jump_label: "跳板",
    import_forward_label: "转发",
    import_forwards_failed: "主机已保存，但转发规则没有写入。",
    pane_split_right: "向右拆分",
    pane_split_down: "向下拆分",
    pane_zoom: "放大窗格",
    pane_unzoom: "显示全部分屏",
    pane_active: "活动",
    pane_pending: "待连接",
    pane_pending_body: "这个窗格在等待。没有启动任何进程，之前的输出也不会显示。",
    pane_connect: "连接",
    pane_remove: "移除窗格",
    pane_close: "关闭窗格",
    pane_resize_less: "缩小窗格",
    pane_resize_more: "放大尺寸",
    pane_move_left: "左移窗格",
    pane_move_right: "右移窗格",
    pane_move_up: "上移窗格",
    pane_move_down: "下移窗格",
    tab_menu_move_left: "标签左移",
    tab_menu_move_right: "标签右移",
    workspace_empty_title: "没有打开的窗格",
    workspace_empty_body: "下一步：打开本地终端，或连接一台主机。关闭这个窗格不会关掉其他会话。",
    pane_missing_host: "这台主机没有保存",
    pane_missing_next: "修复主机或移除这个窗格。没有建立连接。",
    workspace_dangling_tab: "保存的活动标签不在这个工作区里。请另选一个标签，其余标签已保留。",
    workspace_dangling_pane: "保存的活动窗格不在它的标签上。请另选一个窗格，布局已保留。",
    pane_mode_classic: "经典终端",
    pane_mode_integrated: "命令块模式，内容未恢复",
    terminal_mode_classic: "经典网格",
    terminal_mode_blocks: "命令块",
    terminal_block_running: "进行中",
    terminal_block_unknown: "未知",
    terminal_block_exit: "退出码",
    terminal_block_copy: "复制此块",
    terminal_block_rerun: "重新运行",
    terminal_search: "搜索这个终端",
    terminal_search_empty: "这个终端里没有匹配",
    terminal_search_next: "下一个匹配",
    terminal_search_close: "关闭搜索",
    terminal_paste_title: "确认粘贴",
    terminal_paste_target: "目标会话",
    terminal_paste_confirm: "粘贴到这个会话",
    terminal_paste_cancel: "不粘贴",
    terminal_paste_lines: "行",
    terminal_no_blocks: "这个 Shell 没有报告命令块。网格保持原样。",
    terminal_blocks_failed: "Shell 集成已失效。经典网格已保留。",
    terminal_filter_all: "全部命令",
    terminal_filter_running: "仅进行中",
    terminal_filter_unknown: "未知退出",
    terminal_filter_failed: "非零退出",
    terminal_filter_zero: "仅退出码 0",
};

/// A count followed by its unit, in the grammar of the language.
///
/// English takes the plural for anything but one ("2 sessions"); Chinese units
/// already read correctly after any number.
pub fn counted(count: usize, unit: &str, locale: Locale) -> String {
    match locale {
        Locale::English if count != 1 => format!("{count} {unit}s"),
        _ => format!("{count} {unit}"),
    }
}

/// Localized label for a session lifecycle state.
///
/// English reuses the domain label so the two cannot drift; Chinese is written
/// here because the domain crate stays language-neutral.
pub fn session_state_label(state: &SessionState, locale: Locale) -> String {
    match locale {
        Locale::English => state.label().to_owned(),
        Locale::Chinese => match state {
            SessionState::Idle => "未开始",
            SessionState::Connecting => "连接中",
            SessionState::VerifyingHost => "校验主机身份",
            SessionState::Authenticating => "认证中",
            SessionState::Connected => "已连接",
            SessionState::Disconnected(_) => "已断开",
            SessionState::Failed(_) => "失败",
            SessionState::Closed => "已关闭",
        }
        .to_owned(),
    }
}

/// Localized label for a transfer state.
pub fn transfer_state_label(state: &TransferState, locale: Locale) -> String {
    match state {
        TransferState::Queued => match locale {
            Locale::English => "Queued".to_owned(),
            Locale::Chinese => "排队中".to_owned(),
        },
        TransferState::AwaitingOverwrite { .. } => match locale {
            Locale::English => "Waiting for your choice".to_owned(),
            Locale::Chinese => "等待你的选择".to_owned(),
        },
        TransferState::Running { .. } => match locale {
            Locale::English => "Running".to_owned(),
            Locale::Chinese => "进行中".to_owned(),
        },
        TransferState::Completed { .. } => match locale {
            Locale::English => "Completed".to_owned(),
            Locale::Chinese => "已完成".to_owned(),
        },
        TransferState::Failed { .. } => match locale {
            Locale::English => "Failed".to_owned(),
            Locale::Chinese => "失败".to_owned(),
        },
        TransferState::Cancelled { .. } => match locale {
            Locale::English => "Cancelled".to_owned(),
            Locale::Chinese => "已取消".to_owned(),
        },
    }
}

/// The next step an SSH failure suggests, in the interface language.
///
/// The SSH layer words its advice in English and does not know the interface language;
/// its sentence is the key here. A sentence this table does not know is shown as it
/// came, so a new hint is never hidden, only untranslated until it is added (a test
/// checks that every hint the SSH layer has today is here).
#[must_use]
pub fn next_step_text(english: &str, locale: Locale) -> &str {
    match locale {
        Locale::English => english,
        Locale::Chinese => match english {
            "Check the address and port, then connect again." => "检查地址和端口，然后重新连接。",
            "Check the network and the address, then try again." => "检查网络和地址，然后重试。",
            "Check the address, then try again." => "检查地址，然后重试。",
            "The connection stops here; nothing was stored." => "连接到此为止，没有保存任何内容。",
            "Connect again and accept or refuse the identity while it is shown." => {
                "重新连接，并在身份信息显示时选择接受或拒绝。"
            }
            "Do not continue. Check the server identity before trusting it again." => {
                "不要继续。重新信任之前，先核实服务器身份。"
            }
            "The server key cannot be compared; connect with a client that supports it." => {
                "无法比对这台服务器的密钥；请改用支持它的客户端连接。"
            }
            "Check the credentials, or change the authentication method." => {
                "检查凭据，或者换一种登录方式。"
            }
            "Choose a key file this account can read." => "选择一个本账号可以读取的私钥文件。",
            "Enter the key passphrase to continue." => "输入私钥口令后继续。",
            "Enter the correct passphrase for this key." => "输入这个私钥的正确口令。",
            "Choose a key file Shellune can read." => "选择一个 Shellune 能识别的私钥文件。",
            "Start the system SSH agent, then connect again." => {
                "启动系统 SSH Agent，然后重新连接。"
            }
            "Load a key into the agent, then connect again." => {
                "把私钥加载到 Agent 中，然后重新连接。"
            }
            "Use another identity, or a key file instead." => "换一个身份，或改用私钥文件。",
            "Load an identity the server accepts." => "加载一个服务器接受的身份。",
            "The server refused a terminal; use another account." => {
                "服务器拒绝分配终端；请换一个账号。"
            }
            "Connect again to open a new session." => "重新连接以打开新的会话。",
            "Fix the jump chain or the ProxyCommand, then connect again." => {
                "修正跳板链或 ProxyCommand，然后重新连接。"
            }
            "Connect again to start a new attempt." => "重新连接以开始一次新的尝试。",
            "Confirm the command to run it, or remove the ProxyCommand from the host." => {
                "确认后才会运行这条命令，或者从主机上去掉 ProxyCommand。"
            }
            other => other,
        },
    }
}
