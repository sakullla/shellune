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

use crate::app::nav::Platform;

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

    /// The other language.
    pub fn toggle(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|locale| *locale == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// One column of UI copy.
///
/// The struct is large but flat on purpose: a missing field is a compile error in
/// both languages at once, which is cheaper than a runtime fallback table.
#[derive(Debug)]
pub struct Strings {
    /// Name of the navigation group.
    pub workspace: &'static str,
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
    /// Label above the list of rows.
    pub object_label: &'static str,
    /// What the section shortcuts do, shown after the platform's key range.
    ///
    /// The key range itself is not stored here: it depends on the platform and is
    /// built by [`nav_hint`] from [`crate::app::nav::Platform`].
    pub hint_nav_action: &'static str,
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
    /// Hint shown when nothing is running at all.
    pub nothing_running: &'static str,
    /// Label of the shell picker above the terminal.
    pub shell_picker_label: &'static str,
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
}

/// English copy.
pub static ENGLISH: Strings = Strings {
    workspace: "Workspace",
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
    primary_sessions: "Open session",
    primary_transfers: "Start a transfer",
    unavailable_notice: "This action is not available in this build, so nothing was started.",
    object_label: "Current object",
    hint_nav_action: "switches sections",
    zoom_control: "Text size",
    language_control: "Language",
    count_hosts: "host",
    count_sessions: "session",
    count_transfers: "transfer",
    local_shell_fallback: "System shell",
    nothing_running: "Nothing is running yet.",
    shell_picker_label: "Shell",
    shell_custom_button: "Custom path",
    shell_custom_placeholder: "Program name or full path, then Enter",
    shell_none_found: "No shell was found on this machine. Enter a program path to start one.",
    terminal_live: "Running",
    terminal_failed: "Could not start",
    terminal_exited: "Exited",
    terminal_terminated: "Terminated",
    terminal_copy: "Copy",
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
    files_terminal_note: "The terminal keeps running; only this file view is affected.",
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
};

/// Chinese copy.
pub static CHINESE: Strings = Strings {
    workspace: "工作区",
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
    primary_sessions: "打开会话",
    primary_transfers: "开始传输",
    unavailable_notice: "当前版本没有实现这个操作，因此没有启动任何任务。",
    object_label: "当前对象",
    hint_nav_action: "切换分区",
    zoom_control: "文字大小",
    language_control: "语言",
    count_hosts: "台主机",
    count_sessions: "个会话",
    count_transfers: "个传输",
    local_shell_fallback: "系统 Shell",
    nothing_running: "暂无正在运行的任务。",
    shell_picker_label: "Shell 程序",
    shell_custom_button: "自定义路径",
    shell_custom_placeholder: "输入程序名或完整路径后按回车",
    shell_none_found: "本机没有找到可用的 Shell。请输入程序路径来启动一个。",
    terminal_live: "运行中",
    terminal_failed: "启动失败",
    terminal_exited: "已退出",
    terminal_terminated: "被终止",
    terminal_copy: "复制",
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
    files_terminal_note: "终端仍在运行，只有这个文件视图受影响。",
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
};

/// Hint for the section shortcuts, built for one platform.
///
/// The key range comes from [`Platform::shortcut_range`], which derives it from the
/// same modifier and digits the keymap binds, so a macOS build reads "⌘1-4"
/// instead of a hard-coded "Ctrl+1-4".
pub fn nav_hint(strings: &Strings, platform: Platform) -> String {
    format!("{} {}", platform.shortcut_range(), strings.hint_nav_action)
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
